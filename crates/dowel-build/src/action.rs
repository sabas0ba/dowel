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
/// バックエンド（ninja / make）はコマンドをシェルに渡すだけで、作業
/// ディレクトリを指定する術を持たない。行の側で言う——`Step::command_line`
/// と綴りを1つに保つため、ここに置く。
pub fn command_line(command: &[String], cwd: Option<&std::path::Path>) -> String {
    let line = command.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
    match cwd {
        Some(dir) => format!("cd {} && {line}", shell_quote(&dir.display().to_string())),
        None => line,
    }
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
    fn strings_with_spaces_or_quotes_are_quoted() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
    }
}
