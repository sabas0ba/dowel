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
}

impl ActionKind {
    pub fn name(self) -> &'static str {
        match self {
            ActionKind::Compile => "cc",
            ActionKind::Archive => "ar",
            ActionKind::Link => "link",
        }
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
        self.command().iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
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
    fn 安全な文字列は引用しない() {
        assert_eq!(shell_quote("-O2"), "-O2");
        assert_eq!(shell_quote("/usr/bin/cc"), "/usr/bin/cc");
        assert_eq!(shell_quote("-DFOO=1"), "-DFOO=1");
    }

    #[test]
    fn 空白と引用符を含む文字列を引用する() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
    }
}
