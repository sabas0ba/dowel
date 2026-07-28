//! `dowel.toml` の読み取り。
//!
//! 厳密な TOML として維持する（[ADR-0003]）。式の出現は `dowel_eval::strict` が拒否する。
//!
//! [ADR-0003]: ../../../docs/adr/0003-manifest-split.md

use crate::target::PackageId;
use dowel_eval::{Document, Site};
use dowel_support::{Diagnostic, FileId, Label};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Package {
    pub id: PackageId,
    pub name: String,
    pub version: String,
    /// マニフェストの置かれたディレクトリ。`dir()` / `glob()` の基準点。
    pub root: PathBuf,
    pub manifest_file: FileId,
    pub build_file: Option<FileId>,
    pub deps: Vec<Dependency>,
    /// 機能フラグ名 → それが有効化する他の機能
    pub features: BTreeMap<String, Vec<String>>,
    /// `[features]` の見出し。宣言されていない名前を指す診断が参照する
    pub features_site: Option<Site>,
    /// `[toolchain] c = "..."`
    pub toolchain_c: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Dependency {
    pub name: String,
    pub kind: DepKind,
    pub optional: bool,
    pub site: Site,
}

#[derive(Clone, Debug)]
pub enum DepKind {
    /// ローカルパス依存。現時点で唯一取得を要さない形態
    Path(PathBuf),
    /// 未実装の供給形態。診断済みで、下流はターゲットを見つけられない
    Unsupported(&'static str),
}

/// `dowel.toml` の評価済み文書からパッケージ情報を取り出す。
pub fn from_document(
    id: PackageId,
    doc: &Document,
    root: PathBuf,
    manifest_file: FileId,
    diags: &mut Vec<Diagnostic>,
) -> Package {
    let mut pkg = Package {
        id,
        name: root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".into()),
        version: "0.0.0".into(),
        root,
        manifest_file,
        build_file: None,
        deps: Vec::new(),
        features: BTreeMap::new(),
        features_site: None,
        toolchain_c: None,
    };

    match doc.table(&["package"]) {
        Some(t) => {
            if let Some(e) = t.entry("name") {
                match e.value.as_str() {
                    Some(s) => pkg.name = s.to_string(),
                    None => type_err(diags, e.site, "package.name", "a string"),
                }
            } else {
                diags.push(Diagnostic::error("missing-field", "`[package]` has no `name`").at(
                    manifest_file,
                    t.site.span,
                    "write `name = \"...\"`",
                ));
            }
            if let Some(e) = t.entry("version") {
                match e.value.as_str() {
                    Some(s) => pkg.version = s.to_string(),
                    None => type_err(diags, e.site, "package.version", "a string"),
                }
            }
        }
        None => diags.push(Diagnostic::error("missing-table", "missing `[package]`").at(
            manifest_file,
            dowel_support::Span::EMPTY,
            "`dowel.toml` requires a `[package]` table",
        )),
    }

    if let Some(t) = doc.table(&["toolchain"]) {
        if let Some(e) = t.entry("c") {
            match e.value.as_str() {
                Some(s) => pkg.toolchain_c = Some(s.to_string()),
                None => type_err(diags, e.site, "toolchain.c", "a string"),
            }
        }
    }

    if let Some(t) = doc.table(&["features"]) {
        pkg.features_site = Some(t.site);
        for e in &t.entries {
            let name = e.key.join(".");
            let mut enables = Vec::new();
            match e.value.as_list() {
                Some(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(s) => enables.push(s.to_string()),
                            None => type_err(
                                diags,
                                e.site,
                                &format!("features.{name}"),
                                "an array of strings",
                            ),
                        }
                    }
                }
                None => type_err(diags, e.site, &format!("features.{name}"), "an array of strings"),
            }
            pkg.features.insert(name, enables);
        }
    }

    for t in doc.tables_under(&["dependencies"]) {
        if t.path.len() != 1 {
            continue;
        }
        let Some(name_entry) = t.entry("name") else {
            diags.push(Diagnostic::error("missing-field", "dependency has no `name`").at(
                manifest_file,
                t.site.span,
                "write `name = \"...\"`",
            ));
            continue;
        };
        let Some(name) = name_entry.value.as_str().map(|s| s.to_string()) else {
            type_err(diags, name_entry.site, "dependencies.name", "a string");
            continue;
        };
        let optional = t.entry("optional").and_then(|e| e.value.as_bool()).unwrap_or(false);

        let kind = if let Some(e) = t.entry("path") {
            match e.value.as_str() {
                Some(p) => DepKind::Path(PathBuf::from(p)),
                None => {
                    type_err(diags, e.site, "dependencies.path", "a string");
                    DepKind::Unsupported("path")
                }
            }
        } else if t.entry("git").is_some() {
            unsupported(diags, manifest_file, t.site, "git dependencies");
            DepKind::Unsupported("git")
        } else if t.entry("version").is_some() {
            unsupported(diags, manifest_file, t.site, "registry dependencies");
            DepKind::Unsupported("registry")
        } else {
            diags.push(
                Diagnostic::error(
                    "incomplete-dependency",
                    format!("dependency `{name}` has no source"),
                )
                .at(
                    manifest_file,
                    t.site.span,
                    "one of `path`, `version` or `git` is required",
                ),
            );
            DepKind::Unsupported("none")
        };

        pkg.deps.push(Dependency { name, kind, optional, site: t.site });
    }

    pkg
}

fn type_err(diags: &mut Vec<Diagnostic>, site: Site, field: &str, expected: &str) {
    diags.push(Diagnostic::error("type-mismatch", format!("`{field}` must be {expected}")).at(
        site.file,
        site.span,
        format!("write {expected}"),
    ));
}

fn unsupported(diags: &mut Vec<Diagnostic>, file: FileId, site: Site, what: &str) {
    diags.push(
        Diagnostic::error("unsupported-dependency", format!("{what} cannot be fetched yet"))
            .with_label(Label::primary(file, site.span, "this dependency cannot be resolved yet"))
            .note("only `path` dependencies are implemented; fetching is Phase 5 (docs/90-roadmap.md)")
            .note("replacing it with a `path` dependency lets the build proceed"),
    );
}

/// 有効化する機能の集合を求める。
///
/// `default` は明示的に無効化しない限り含める。機能同士の有効化関係は
/// 到達不能になるまで閉じる。
pub fn resolve_features(
    pkg: &Package,
    requested: &[String],
    use_default: bool,
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut stack: Vec<String> = requested.to_vec();
    if use_default {
        if let Some(defaults) = pkg.features.get("default") {
            stack.extend(defaults.iter().cloned());
        }
    }
    while let Some(f) = stack.pop() {
        if f == "default" || !out.insert(f.clone()) {
            continue;
        }
        if let Some(enables) = pkg.features.get(&f) {
            stack.extend(enables.iter().cloned());
        }
    }
    out
}

/// 依存の値が `optional` かつ対応する機能が無効なら取り込まない。
pub fn is_active(dep: &Dependency, features: &std::collections::BTreeSet<String>) -> bool {
    !dep.optional || features.contains(&dep.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg_with(features: &[(&str, &[&str])]) -> Package {
        let mut map = BTreeMap::new();
        for (k, v) in features {
            map.insert(k.to_string(), v.iter().map(|s| s.to_string()).collect());
        }
        Package {
            id: PackageId(0),
            name: "p".into(),
            version: "0".into(),
            root: PathBuf::from("."),
            manifest_file: FileId(0),
            build_file: None,
            deps: Vec::new(),
            features: map,
            features_site: None,
            toolchain_c: None,
        }
    }

    #[test]
    fn pulls_in_default_features() {
        let p = pkg_with(&[("default", &["zlib"]), ("zlib", &[])]);
        let f = resolve_features(&p, &[], true);
        assert!(f.contains("zlib"));
        assert!(!f.contains("default"), "`default` itself is not kept as a feature name");
    }

    #[test]
    fn default_features_can_be_disabled() {
        let p = pkg_with(&[("default", &["zlib"]), ("zlib", &[])]);
        assert!(resolve_features(&p, &[], false).is_empty());
    }

    #[test]
    fn closes_over_chained_features() {
        let p = pkg_with(&[("a", &["b"]), ("b", &["c"]), ("c", &[])]);
        let f = resolve_features(&p, &["a".into()], false);
        assert_eq!(f.iter().cloned().collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }

    #[test]
    fn terminates_on_cyclic_features() {
        let p = pkg_with(&[("a", &["b"]), ("b", &["a"])]);
        let f = resolve_features(&p, &["a".into()], false);
        assert_eq!(f.len(), 2);
    }
}
