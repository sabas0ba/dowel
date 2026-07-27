//! `dowel.toml` を厳密な TOML に保つための検証。
//!
//! 構文は `dowel.build` と共通だが、`dowel.toml` では値の位置に式を許さない
//! （[ADR-0003]）。狙いは SBOM 生成器・脆弱性スキャナ・更新ボットが
//! **本システムの言語を実装せずに**依存一覧を読めることであり、
//! それは「式が出現したら失敗させる」ことで担保できる。
//!
//! [ADR-0003]: ../../../docs/adr/0003-manifest-split.md

use dowel_support::{Diagnostic, FileId};
use dowel_syntax::{Node, NodeKind};

pub fn check(root: &Node, file: FileId) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    walk(root, file, &mut out);
    out
}

fn walk(node: &Node, file: FileId, out: &mut Vec<Diagnostic>) {
    let offending = match node.kind {
        NodeKind::Call => Some(("関数呼び出し", "値をそのまま書く")),
        NodeKind::Match => Some(("`match`", "条件は `when = { os = \"windows\" }` の形で書く")),
        NodeKind::WhenClause => {
            Some(("後置の `when`", "条件は `when = { os = \"windows\" }` の形で書く"))
        }
        NodeKind::NsRef => Some(("構成への参照", "値をそのまま書く")),
        _ => None,
    };
    if let Some((what, hint)) = offending {
        out.push(
            Diagnostic::error(
                "expression-in-strict-toml",
                format!("`dowel.toml` の値の位置に{}は置けない", what),
            )
            .at(file, node.span, "ここには置けない")
            .note("`dowel.toml` は厳密な TOML として維持する。外部ツールが独自パーサなしで読めることを保証するため")
            .note(format!("式が必要な記述は `dowel.build` に置く。{hint}")),
        );
        // 部分木の中をさらに報告しても情報が増えないため、ここで打ち切る。
        return;
    }
    for child in node.nodes() {
        walk(child, file, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_src(src: &str) -> Vec<Diagnostic> {
        let parsed = dowel_syntax::parse(src, FileId(0));
        check(&parsed.root, FileId(0))
    }

    #[test]
    fn 通常の_toml_は通る() {
        let src = r#"
[package]
name    = "libfoo"
version = "0.3.1"

[[dependencies]]
name    = "winsock-shim"
version = "0.2"
when    = { os = "windows" }

[features]
default = ["zlib"]
"#;
        assert!(check_src(src).is_empty());
    }

    #[test]
    fn 関数呼び出しを拒否する() {
        let d = check_src("[package]\nname = dir(\"x\")\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, "expression-in-strict-toml");
    }

    #[test]
    fn match_を拒否する() {
        let d = check_src("[package]\nname = match cfg.opt { _ => \"x\" }\n");
        assert_eq!(d.len(), 1, "部分木の中を重ねて報告しない");
    }

    #[test]
    fn 後置の_when_を拒否する() {
        let d = check_src("[features]\ndefault = [\"zlib\"] when feature.x\n");
        assert_eq!(d.len(), 1);
    }
}
