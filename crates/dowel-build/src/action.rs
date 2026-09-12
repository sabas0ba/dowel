//! アクショングラフ。
//!
//! 1つのアクションは「入力の集合から出力の集合を作る1回のプロセス起動」である。
//! ここで持つ情報は、将来 CAS によるアクションキャッシュへ移す際に
//! そのまま鍵の材料になる（docs/20-architecture.md 8節）。

use dowel_model::TargetId;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ActionId(pub usize);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionKind {
    Compile,
    Archive,
    Link,
    /// 成果物から別の成果物を作る（`artifacts` ブロック、issue #60）
    Transform,
    /// ソースを作る（`generate` ブロック、
    /// [ADR-0054](../../../docs/adr/0054-generated-sources.md)）
    Generate,
}

/// 種別の全て。`name` と `parse` が食い違わないことをこの表で確かめる。
pub const ALL_KINDS: &[ActionKind] = &[
    ActionKind::Compile,
    ActionKind::Archive,
    ActionKind::Link,
    ActionKind::Transform,
    ActionKind::Generate,
];

impl ActionKind {
    pub fn name(self) -> &'static str {
        match self {
            ActionKind::Compile => "cc",
            ActionKind::Archive => "ar",
            ActionKind::Link => "link",
            ActionKind::Transform => "transform",
            ActionKind::Generate => "generate",
        }
    }

    /// 書き出した名前から戻す。独自形式（`build-graph.json`）を読み直すために要る。
    pub fn parse(s: &str) -> Option<ActionKind> {
        ALL_KINDS.iter().copied().find(|k| k.name() == s)
    }
}

#[derive(Clone, Debug)]
pub struct Action {
    pub id: ActionId,
    pub kind: ActionKind,
    pub target: TargetId,
    pub program: String,
    pub args: Vec<String>,
    /// 明示的な入力。すべて絶対パス
    pub inputs: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    /// コンパイラが書き出すヘッダ依存。ninja の `depfile`
    pub depfile: Option<PathBuf>,
    pub description: String,
    /// このアクションより前に完了していなければならないアクション
    pub deps: Vec<ActionId>,
    /// 起動するときの作業ディレクトリ。`None` はビルドディレクトリである。
    ///
    /// 生成だけがこれを使う（ADR-0054）。出力の置き場所を作業ディレクトリに
    /// することで、宣言の側は絶対パスを組み立てずに済む
    pub cwd: Option<PathBuf>,
}

impl Action {
    /// 実行する完全なコマンド列。
    pub fn command(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(self.args.len() + 1);
        v.push(self.program.clone());
        v.extend(self.args.iter().cloned());
        v
    }

    pub fn command_line(&self) -> String {
        command_line(&self.command(), self.cwd.as_deref())
    }
}

/// シェルへ渡す1行。作業ディレクトリが要るなら `cd` を前に置く。
///
/// ninja / make 向けには、作業ディレクトリの指定を `cd` としてコマンドに含める。
/// `Step::command_line` と同じコマンド生成処理を使用するため、ここに集約する。
pub fn command_line(command: &[String], cwd: Option<&std::path::Path>) -> String {
    let line = command.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
    match cwd {
        Some(dir) => format!("cd {} && {line}", shell_quote(&dir.display().to_string())),
        None => line,
    }
}

/// コマンドやパスに含まれる改行文字を検出する
/// （[ADR-0058](../../../docs/adr/0058-a-command-a-backend-cannot-spell.md)）。
///
/// ninja の変数値と make のレシピは行単位で記述するため、改行を含む引数を
/// そのまま出力できない。以前の ninja 出力は改行を空白に置換していたため、
/// `printf` で2行のマクロ定義を生成するコマンドが1行の定義を出力していた。
pub fn breaks_the_line(text: &str) -> Option<char> {
    text.chars().find(|c| *c == '\n' || *c == '\r')
}

/// 診断用の文字表現。改行と復帰は制御文字そのものではなく名称を返す。
pub fn show_char(c: char) -> String {
    match c {
        '\n' => "a newline".to_string(),
        '\r' => "a carriage return".to_string(),
        c => format!("`{c}`"),
    }
}

/// バックエンドで表現できない文字の診断と、修正方法を生成する（ADR-0058）。
///
/// マニフェストの `\n` は解析時に改行へ変換される。`printf` に2文字の
/// `\` と `n` を渡す意図なら、`\\n` と記述する必要がある。
/// このエスケープ方法を診断に含める。
pub fn cannot_spell(who: &str, place: &str, c: char, description: &str) -> String {
    let mut reason = format!(
        "{who} cannot spell {} inside {place}, and `{description}` contains one",
        show_char(c)
    );
    if let Some(escape) = match c {
        '\n' => Some("\\n"),
        '\r' => Some("\\r"),
        _ => None,
    } {
        reason.push_str(&format!(
            ". if the program is meant to receive the two characters `{escape}`, \
             write `\\{escape}` — the manifest turns `{escape}` into the character itself"
        ));
    }
    // direct は引数をプロセスへ直接渡すため、行単位の出力形式による制約がない。
    // 実際の改行をコマンドやパスに含める場合の実行方法として案内する。
    reason.push_str(". `--backend=direct` runs the steps itself, with nothing spelled on one line");
    reason
}

/// POSIX シェル向けの引用。ninja も `compile_commands.json` も
/// 最終的にシェルへ渡すため、1箇所で行う。
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '=' | '+' | ':' | ',')
        })
    {
        return s.to_string();
    }
    // 単引用符の中では単引用符自身のみを外に出す。
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_survives_a_round_trip_through_its_name() {
        for k in ALL_KINDS {
            assert_eq!(ActionKind::parse(k.name()), Some(*k));
        }
        assert_eq!(ActionKind::parse("nope"), None);
    }

    #[test]
    fn safe_strings_are_left_unquoted() {
        assert_eq!(shell_quote("-O2"), "-O2");
        assert_eq!(shell_quote("/usr/bin/cc"), "/usr/bin/cc");
        assert_eq!(shell_quote("-DFOO=1"), "-DFOO=1");
    }

    #[test]
    fn refusing_a_newline_names_the_escape_that_was_probably_meant() {
        // バックエンドの変更に加え、元のバックエンドで実行するための
        // エスケープ方法も診断に含める（ADR-0058）。
        let r = cannot_spell("ninja", "a build edge", '\n', "GEN table");
        assert!(r.contains("a newline"), "{r}");
        assert!(r.contains("write `\\\\n`"), "{r}");
        assert!(r.contains("--backend=direct"), "{r}");
    }

    #[test]
    fn refusing_something_that_has_no_escape_offers_none() {
        let r = cannot_spell("make", "a recipe", '\u{7}', "GEN table");
        assert!(!r.contains("two characters"), "{r}");
        assert!(r.contains("--backend=direct"), "{r}");
    }

    #[test]
    fn strings_with_spaces_or_quotes_are_quoted() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
    }
}
