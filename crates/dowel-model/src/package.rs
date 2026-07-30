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
    /// 無印の `[toolchain]`。ホスト向けビルドに適用される
    pub toolchain: ToolchainDecl,
    /// `[toolchain.<triple>]`。ターゲットトリプルごとの宣言。
    /// `[runner.<triple>]` と同じ形で、`--target` の切り替えに追随する（issue #42）
    pub toolchains: BTreeMap<String, ToolchainDecl>,
}

/// 1つの `[toolchain]`（または `[toolchain.<triple>]`）テーブルの内容。
#[derive(Clone, Debug, Default)]
pub struct ToolchainDecl {
    /// テーブル見出しの位置。宣言に由来する診断が参照する
    pub site: Option<Site>,
    /// `c = "..."`
    pub c: Option<String>,
    /// その宣言が書かれた位置。実在しないツールチェーンを指す診断が参照する
    pub c_site: Option<Site>,
    /// `cxx = "..."`
    pub cxx: Option<String>,
    /// その宣言が書かれた位置
    pub cxx_site: Option<Site>,
}

impl Package {
    /// `triple` 向けのビルドに適用されるツールチェーン宣言。
    ///
    /// トリプルごとの宣言が最優先。無印の `[toolchain]` はホスト向けの宣言で
    /// あり、別トリプルのビルドには適用しない。ホストのコンパイラで組んだ
    /// 成果物に別トリプルの名前が付くのが issue #42 の形である。
    /// 別トリプルに宣言が無い場合は `None` を返し、呼び手が拒む。
    pub fn toolchain_for(&self, triple: &str, host: &str) -> Option<&ToolchainDecl> {
        if let Some(d) = self.toolchains.get(triple) {
            return Some(d);
        }
        if triple == host {
            return Some(&self.toolchain);
        }
        None
    }
}

#[derive(Clone, Debug)]
pub struct Dependency {
    pub name: String,
    pub kind: DepKind,
    pub optional: bool,
    /// `[[dependencies]]` の見出し
    pub site: Site,
    /// 供給元を書いた行（`path` / `git` / `version`）。無ければ見出しと同じ。
    ///
    /// 依存が多段になると、読めなかったパスだけでは「どの `dowel.toml` に
    /// 書かれた宣言か」が分からない。`path` は相対で書かれるため、
    /// パスから遡るのも一手間になる。
    pub source_site: Site,
}

#[derive(Clone, Debug)]
pub enum DepKind {
    /// ローカルパス依存。取得を要さない形態
    Path(PathBuf),
    /// git 依存。`rev` はフル 40 桁の commit sha（読み取り時に検証済み）。
    /// ブランチ・タグでの解決は許さない。名前だけの参照は固定とみなさない
    /// （docs/11-toml-reference.md）
    Git { url: String, rev: String },
    /// `version` 依存。システムの pkg-config で解決する（ADR-0015）。
    /// 値は版の下限
    PkgConfig { min_version: String },
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
        toolchain: ToolchainDecl::default(),
        toolchains: BTreeMap::new(),
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

    for t in doc.tables_under(&["toolchain"]) {
        // `[toolchain]` はホスト向け、`[toolchain.<triple>]` はそのトリプル向け。
        let (label, triple) = match t.path.len() {
            1 => ("toolchain".to_string(), None),
            2 => (format!("toolchain.{}", t.path[1]), Some(t.path[1].clone())),
            _ => {
                diags.push(
                    Diagnostic::error(
                        "unknown-table",
                        format!("`[{}]` is not a toolchain declaration", t.path.join(".")),
                    )
                    .at(
                        manifest_file,
                        t.site.span,
                        "write `[toolchain]` or `[toolchain.<triple>]`",
                    ),
                );
                continue;
            }
        };
        let mut decl = ToolchainDecl { site: Some(t.site), ..ToolchainDecl::default() };
        if let Some(e) = t.entry("c") {
            match e.value.as_str() {
                Some(s) => {
                    decl.c = Some(s.to_string());
                    decl.c_site = Some(e.site);
                }
                None => type_err(diags, e.site, &format!("{label}.c"), "a string"),
            }
        }
        if let Some(e) = t.entry("cxx") {
            match e.value.as_str() {
                Some(s) => {
                    decl.cxx = Some(s.to_string());
                    decl.cxx_site = Some(e.site);
                }
                None => type_err(diags, e.site, &format!("{label}.cxx"), "a string"),
            }
        }
        match triple {
            Some(triple) => {
                // トリプル向けの宣言は、そのトリプルのビルド全体を担う。
                // `c` が無いと C のコンパイルとリンクがホストの既定へ落ち、
                // 成果物のアーキテクチャが黙って食い違う（issue #42）。
                if decl.c.is_none() {
                    diags.push(
                        Diagnostic::error(
                            "missing-field",
                            format!("toolchain `{triple}` has no `c`"),
                        )
                        .at(
                            manifest_file,
                            t.site.span,
                            "a target toolchain must name its C compiler",
                        )
                        .note("for example `c = \"aarch64-linux-gnu-gcc\"`"),
                    );
                }
                pkg.toolchains.insert(triple, decl);
            }
            None => pkg.toolchain = decl,
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

        let source_site = ["path", "git", "version"]
            .iter()
            .find_map(|k| t.entry(k))
            .map(|e| e.site)
            .unwrap_or(t.site);

        let kind = if let Some(e) = t.entry("path") {
            match e.value.as_str() {
                Some(p) => DepKind::Path(PathBuf::from(p)),
                None => {
                    type_err(diags, e.site, "dependencies.path", "a string");
                    DepKind::Unsupported("path")
                }
            }
        } else if let Some(e) = t.entry("git") {
            match e.value.as_str() {
                Some(url) => match pinned_rev(t) {
                    Ok(rev) => DepKind::Git { url: url.to_string(), rev },
                    Err(found) => {
                        unpinned(diags, manifest_file, t, &name, found);
                        DepKind::Unsupported("git")
                    }
                },
                None => {
                    type_err(diags, e.site, "dependencies.git", "a string");
                    DepKind::Unsupported("git")
                }
            }
        } else if let Some(e) = t.entry("version") {
            match e.value.as_str() {
                Some(v) => DepKind::PkgConfig { min_version: v.to_string() },
                None => {
                    type_err(diags, e.site, "dependencies.version", "a string");
                    DepKind::Unsupported("version")
                }
            }
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

        pkg.deps.push(Dependency { name, kind, optional, site: t.site, source_site });
    }

    pkg
}

/// `rev` がフル 40 桁の commit sha であることを確かめる。
///
/// 短縮形やブランチ名を受けると、同じマニフェストが時間や環境で別の内容に
/// 解決されうる。「名前だけの参照は固定とみなさない」（docs/50-development.md
/// 5節）を依存にも課す。
fn pinned_rev(t: &dowel_eval::Table) -> Result<String, Option<String>> {
    let Some(e) = t.entry("rev") else { return Err(None) };
    let Some(s) = e.value.as_str() else { return Err(Some(String::new())) };
    let rev = s.to_ascii_lowercase();
    if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(rev)
    } else {
        Err(Some(s.to_string()))
    }
}

fn unpinned(
    diags: &mut Vec<Diagnostic>,
    file: FileId,
    t: &dowel_eval::Table,
    name: &str,
    found: Option<String>,
) {
    let what = match &found {
        None => "has no `rev`".to_string(),
        Some(s) if s.is_empty() => "has a non-string `rev`".to_string(),
        Some(s) => format!("pins `rev = {s:?}`, which is not a full commit sha"),
    };
    let site = t.entry("rev").map(|e| e.site).unwrap_or(t.site);
    diags.push(
        Diagnostic::error("unpinned-dependency", format!("git dependency `{name}` {what}"))
            .with_label(Label::primary(
                file,
                site.span,
                "a full 40-digit commit sha is required",
            ))
            .note("branches, tags, and abbreviated shas resolve differently over time; they do not count as pinned")
            .note("take the sha with `git rev-parse HEAD` in the dependency's repository"),
    );
}

fn type_err(diags: &mut Vec<Diagnostic>, site: Site, field: &str, expected: &str) {
    diags.push(Diagnostic::error("type-mismatch", format!("`{field}` must be {expected}")).at(
        site.file,
        site.span,
        format!("write {expected}"),
    ));
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
            toolchain: ToolchainDecl::default(),
            toolchains: BTreeMap::new(),
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
    fn the_toolchain_is_selected_by_the_target_triple() {
        const HOST: &str = "x86_64-unknown-linux-gnu";
        const CROSS: &str = "riscv64gc-unknown-linux-gnu";
        let mut p = pkg_with(&[]);
        p.toolchain.c = Some("cc".into());
        p.toolchains.insert(
            CROSS.into(),
            ToolchainDecl { c: Some("riscv64-gcc".into()), ..Default::default() },
        );

        // ホストのビルドは無印の宣言、別トリプルはそのトリプルの宣言。
        assert_eq!(p.toolchain_for(HOST, HOST).unwrap().c.as_deref(), Some("cc"));
        assert_eq!(p.toolchain_for(CROSS, HOST).unwrap().c.as_deref(), Some("riscv64-gcc"));
    }

    #[test]
    fn a_foreign_triple_without_a_declaration_resolves_to_nothing() {
        let mut p = pkg_with(&[]);
        p.toolchain.c = Some("cc".into());
        // 無印の宣言はホスト向けであり、別トリプルのビルドへは落ちない。
        // ここが `Some` になると、ホストの成果物に別トリプルの名前が付く（issue #42）。
        assert!(p
            .toolchain_for("riscv64gc-unknown-linux-gnu", "x86_64-unknown-linux-gnu")
            .is_none());
    }

    #[test]
    fn terminates_on_cyclic_features() {
        let p = pkg_with(&[("a", &["b"]), ("b", &["a"])]);
        let f = resolve_features(&p, &["a".into()], false);
        assert_eq!(f.len(), 2);
    }
}
