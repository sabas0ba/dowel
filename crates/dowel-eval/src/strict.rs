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
        NodeKind::Call => Some(("a function call", "write the value directly")),
        NodeKind::Match => Some(("`match`", "write conditions as `when = { os = \"windows\" }`")),
        NodeKind::WhenClause => {
            Some(("a postfix `when`", "write conditions as `when = { os = \"windows\" }`"))
        }
        NodeKind::NsRef => Some(("a configuration reference", "write the value directly")),
        _ => None,
    };
    if let Some((what, hint)) = offending {
        out.push(
            Diagnostic::error(
                "expression-in-strict-toml",
                format!("{what} cannot appear in a value position in `dowel.toml`"),
            )
            .at(file, node.span, "not allowed here")
            .note("`dowel.toml` stays strict TOML so third-party tools can read it without implementing this language")
            .note(format!("put anything that needs an expression in `dowel.build`. {hint}")),
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
    fn plain_toml_is_accepted() {
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
    fn rejects_function_calls() {
        let d = check_src("[package]\nname = dir(\"x\")\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, "expression-in-strict-toml");
    }

    #[test]
    fn rejects_match() {
        let d = check_src("[package]\nname = match cfg.opt { _ => \"x\" }\n");
        assert_eq!(d.len(), 1, "nested occurrences must not be reported twice");
    }

    #[test]
    fn rejects_postfix_when() {
        let d = check_src("[features]\ndefault = [\"zlib\"] when feature.x\n");
        assert_eq!(d.len(), 1);
    }
}
