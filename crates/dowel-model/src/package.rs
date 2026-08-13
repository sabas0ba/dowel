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
    /// `[package] description`。一行の説明。空なら名前で代える
    pub description: String,
    /// マニフェストの置かれたディレクトリ。`dir()` / `glob()` の基準点。
    pub root: PathBuf,
    pub manifest_file: FileId,
    pub build_file: Option<FileId>,
    pub deps: Vec<Dependency>,
    /// 機能フラグ名 → それが有効化する他の機能
    pub features: BTreeMap<String, Vec<String>>,
    /// `[features]` の見出し。宣言されていない名前を指す診断が参照する
    pub features_site: Option<Site>,
    /// 同時に立ててはならない機能の組（`[features] exclusive`、issue #82）。
    /// 宣言の位置つき。機能は加算のままで、排他は**宣言された制約**である
    pub exclusive: Vec<(Vec<String>, Site)>,
    /// `[package] targets`。この木が対象とするトリプル（issue #71）。
    /// 空は「宣言なし」であり、どのトリプルでも組める
    pub targets: Vec<String>,
    /// `targets = [...]` が書かれた位置
    pub targets_site: Option<Site>,
    /// 無印の `[toolchain]`。ホスト向けビルドに適用される
    pub toolchain: ToolchainDecl,
    /// `[toolchain.<triple>]`。ターゲットトリプルごとの宣言。
    /// `[runner.<triple>]` と同じ形で、`--target` の切り替えに追随する（issue #42）
    pub toolchains: BTreeMap<String, ToolchainDecl>,
    /// `[package] toolchains`。共有の記述ファイルへの相対パスと、その位置
    /// （[ADR-0033](../../../docs/adr/0033-shared-toolchain-file.md)）
    pub toolchains_path: Option<(String, Site)>,
}

/// 1つの `[toolchain]`（または `[toolchain.<triple>]`）テーブルの内容。
///
/// 受け付ける道具は `dowel_eval::config::TOOLS` の表が決める。
/// 道具ごとの個別フィールドにしないのは、道具を増やすとき（例: disasm）に
/// 触る箇所を表1行に留めるためである（issue #50 のレビュー）。
#[derive(Clone, Debug, Default)]
pub struct ToolchainDecl {
    /// テーブル見出しの位置。宣言に由来する診断が参照する
    pub site: Option<Site>,
    /// 道具名 → 宣言。宣言の無い道具は表の既定に落ちる
    tools: BTreeMap<String, ToolDecl>,
    /// 引数の綴り方（[ADR-0027](../../../docs/adr/0027-toolchain-style.md)）。
    /// 宣言が無ければ三つ組から導く
    pub style: Option<dowel_eval::config::Style>,
    /// 取ってくる道具一式（[ADR-0044](../../../docs/adr/0044-toolchain-acquisition.md)）。
    /// 宣言が無ければ、道具は機械に既に在るものとして探す
    pub source: Option<ToolchainSource>,
    /// sysroot（[ADR-0047](../../../docs/adr/0047-sysroot.md)）。
    /// `sysroot()` の基準点。相対なら取ってきた道具一式の根から解く
    pub sysroot: Option<String>,
}

/// 取ってくる道具一式の出所（ADR-0044）。
///
/// 固定の形は書庫依存（[ADR-0029](../../../docs/adr/0029-tarball-dependencies.md)）
/// と同じである——URL は名前であり、名前の裏のバイトは変わりうる。
#[derive(Clone, Debug)]
pub struct ToolchainSource {
    pub url: String,
    pub sha256: String,
    /// `url` が書かれた位置
    pub site: Site,
}

/// 1つの道具の宣言。
#[derive(Clone, Debug)]
pub struct ToolDecl {
    /// 起動するコマンド
    pub command: String,
    /// 宣言が書かれた位置。実在しない道具を指す診断が参照する
    pub site: Site,
}

impl ToolchainDecl {
    /// 道具の宣言。名前は `dowel_eval::config::TOOLS` のもの。
    pub fn tool(&self, name: &str) -> Option<&ToolDecl> {
        self.tools.get(name)
    }

    pub fn set_tool(&mut self, name: &str, command: String, site: Site) {
        self.tools.insert(name.to_string(), ToolDecl { command, site });
    }

    /// 自分に無いものを `other` から補う。自分に在るものは動かさない。
    ///
    /// 共有の記述ファイル（[ADR-0033](../../../docs/adr/0033-shared-toolchain-file.md)）
    /// を後から読むための向きである。補う単位は道具1つ——三つ組ごとにすると
    /// 「この機械では C だけ別」のために表全体を写し直すことになる。
    pub fn fill_from(&mut self, other: &ToolchainDecl) {
        for (name, decl) in &other.tools {
            self.tools.entry(name.clone()).or_insert_with(|| decl.clone());
        }
        if self.style.is_none() {
            self.style = other.style;
        }
        if self.source.is_none() {
            self.source = other.source.clone();
        }
        if self.sysroot.is_none() {
            self.sysroot = other.sysroot.clone();
        }
        if self.site.is_none() {
            self.site = other.site;
        }
    }

    /// 無印の `[toolchain]` 用。まだ何も宣言されていなければ丸ごと入れる。
    fn fill_from_or_replace(&mut self, other: ToolchainDecl) {
        if self.site.is_none() && self.tools.is_empty() && self.style.is_none() {
            *self = other;
        } else {
            self.fill_from(&other);
        }
    }
}

impl Package {
    /// `triple` 向けのビルドに適用されるツールチェーン宣言。
    ///
    /// トリプルごとの宣言が最優先。無印の `[toolchain]` はホスト向けの宣言で
    /// あり、別トリプルのビルドには適用しない。ホストのコンパイラで組んだ
    /// 成果物に別トリプルの名前が付くのが issue #42 の形である。
    /// 別トリプルに宣言が無い場合は `None` を返し、呼び手が拒む。
    /// `is_host` は「その三つ組がこの機械を指すか」。
    ///
    /// 綴りの一致で判定させない。ホストには綴りが2つある——dowel が組み立てる
    /// 近似と、C コンパイラが名乗るもの（[ADR-0028](../../../docs/adr/0028-probe-facts.md)）
    /// ——ので、どちらか一方と比べると、もう一方がクロス扱いになる。判定は
    /// `Config::targets_host` が持つ。
    pub fn toolchain_for(&self, triple: &str, is_host: bool) -> Option<&ToolchainDecl> {
        if let Some(d) = self.toolchains.get(triple) {
            return Some(d);
        }
        if is_host {
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
    /// 書庫の取得（[ADR-0029](../../../docs/adr/0029-tarball-dependencies.md)）。
    /// `sha256` は 64 桁の16進で、内容そのものを固定する
    Tarball { url: String, sha256: String },
    /// 未実装の供給形態。診断済みで、下流はターゲットを見つけられない
    Unsupported(&'static str),
}

/// `dowel.toml` が読む最上位のテーブル。
const KNOWN_TABLES: &[&str] = &["package", "toolchain", "dependencies", "features"];

/// `[toolchain]` で道具ではないキー。引数の綴り方を選ぶ（ADR-0027）。
pub const STYLE_KEY: &str = "style";

/// 予約済みで、まだ読まないテーブル（docs/11-toml-reference.md）。
/// 拒まないのは、書いてあっても無視すると**文書で述べてある**ためである。
const RESERVED_TABLES: &[&str] = &["policy"];

/// `dowel.build` の側の語彙。置き場所を間違えたときに、どこへ書くかを述べる。
const BUILD_TABLES: &[&str] = &["runner", "lib", "bin", "test", "bench", "template"];

/// 最上位のテーブル名を検査する。
///
/// 読まないテーブルを黙って読み飛ばすと、書いたはずの宣言が記録の外に落ちる。
/// `[runner.<triple>]` を `dowel.toml` に書く形が典型で、`[toolchain.<triple>]`
/// のすぐ隣に書きたくなるうえ、`missing-runner` が「宣言が無い」と言うため、
/// 利用者は書いてあるものを見ながら途方に暮れる（issue #74）。
fn check_tables(doc: &Document, manifest_file: FileId, diags: &mut Vec<Diagnostic>) {
    for t in &doc.tables {
        let Some(head) = t.path.first() else { continue };
        if KNOWN_TABLES.contains(&head.as_str()) || RESERVED_TABLES.contains(&head.as_str()) {
            continue;
        }
        let mut d = Diagnostic::error(
            "unknown-table",
            format!("`[{}]` is not read from `dowel.toml`", t.path.join(".")),
        )
        .at(manifest_file, t.site.span, "this table has no meaning here");
        if BUILD_TABLES.contains(&head.as_str()) {
            d = d.note(format!("`{head}` tables are declared in `dowel.build`"));
        } else {
            d = d.note(format!("`dowel.toml` reads: {}", KNOWN_TABLES.join(", ")));
            if let (Some(c), Some(&span)) = (
                dowel_support::diag::closest(head, KNOWN_TABLES.iter().copied()),
                t.path_spans.first(),
            ) {
                d = d.suggest(manifest_file, span, c, format!("did you mean `{c}`?"));
            }
        }
        diags.push(d);
    }
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
        description: String::new(),
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
        exclusive: Vec::new(),
        targets: Vec::new(),
        targets_site: None,
        toolchain: ToolchainDecl::default(),
        toolchains: BTreeMap::new(),
        toolchains_path: None,
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
            // 対象とするトリプル。宣言は道具の宣言とは別の事柄である——
            // ホスト向けにも組めるが、クロスのときだけ道具を替えたい木は
            // `[toolchain.<triple>]` を持ちつつ対象を絞らない（issue #71）。
            if let Some(e) = t.entry("targets") {
                pkg.targets_site = Some(e.site);
                match &e.value.data {
                    dowel_eval::Data::List(items) => {
                        for item in items {
                            match item.as_str() {
                                Some(s) => pkg.targets.push(s.to_string()),
                                None => {
                                    type_err(diags, e.site, "package.targets", "a list of strings")
                                }
                            }
                        }
                    }
                    _ => type_err(diags, e.site, "package.targets", "a list of strings"),
                }
            }
            // 一行の説明。pkg-config の記述は `Description` を要求するので、
            // 書ける場所が要る（[ADR-0043](../../../docs/adr/0043-pkgconfig-generation.md)）。
            if let Some(e) = t.entry("description") {
                match e.value.as_str() {
                    Some(s) => pkg.description = s.to_string(),
                    None => type_err(diags, e.site, "package.description", "a string"),
                }
            }
            // 共有の toolchain 記述ファイル（ADR-0033）。読み込みは
            // `Session` が行う——クエリ経由で読まないと、ファイルを直しても
            // 再評価されない。
            if let Some(e) = t.entry("toolchains") {
                match e.value.as_str() {
                    Some(s) => pkg.toolchains_path = Some((s.to_string(), e.site)),
                    None => type_err(diags, e.site, "package.toolchains", "a string"),
                }
            }
        }
        None => diags.push(Diagnostic::error("missing-table", "missing `[package]`").at(
            manifest_file,
            dowel_support::Span::EMPTY,
            "`dowel.toml` requires a `[package]` table",
        )),
    }

    check_tables(doc, manifest_file, diags);

    read_toolchains(&mut pkg, doc, manifest_file, diags);

    if let Some(t) = doc.table(&["features"]) {
        pkg.features_site = Some(t.site);
        for e in &t.entries {
            let name = e.key.join(".");
            // `exclusive` は機能名ではなく制約の宣言である。`default` と同じく
            // この表の予約キーとして扱う（issue #82）。
            if name == EXCLUSIVE {
                read_exclusive(&mut pkg, e, diags);
                continue;
            }
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
        check_exclusive_names(&mut pkg, diags);
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

        let source_site =
            SOURCE_KEYS.iter().find_map(|(k, _)| t.entry(k)).map(|e| e.site).unwrap_or(t.site);

        // 出所を2つ以上名乗る項目は、片方が読まれない。読まれない宣言を
        // 黙って受けると、どちらが使われたのかがマニフェストから読めない
        // （issue #79）。
        if let Some(d) = conflicting_sources(t, manifest_file, &name) {
            diags.push(d);
            pkg.deps.push(Dependency {
                name,
                kind: DepKind::Unsupported("conflict"),
                optional,
                site: t.site,
                source_site,
            });
            continue;
        }

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
        } else if let Some(e) = t.entry("url") {
            match e.value.as_str() {
                Some(url) => match declared_sha256(t) {
                    Ok(sha256) => DepKind::Tarball { url: url.to_string(), sha256 },
                    Err(found) => {
                        unhashed(diags, manifest_file, t, &name, found);
                        DepKind::Unsupported("url")
                    }
                },
                None => {
                    type_err(diags, e.site, "dependencies.url", "a string");
                    DepKind::Unsupported("url")
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
                    "one of `path`, `git`, `url` or `version` is required",
                ),
            );
            DepKind::Unsupported("none")
        };

        pkg.deps.push(Dependency { name, kind, optional, site: t.site, source_site });
    }

    pkg
}

/// `[toolchain]` / `[toolchain.<triple>]` を読む。
///
/// `dowel.toml` からも共有の記述ファイルからも同じ読み方をする
/// （[ADR-0033](../../../docs/adr/0033-shared-toolchain-file.md)）。
///
/// **既に在る宣言は上書きしない。** 呼ぶ順が優先順位であり、`dowel.toml`
/// を先に読むことで、そこに書いたものが共有ファイルより勝つ。上書きの
/// 単位は道具1つである——三つ組ごとにすると「この機械では C だけ別」の
/// ために表全体を写し直すことになり、写しを減らす目的に反する。
pub fn read_toolchains(
    pkg: &mut Package,
    doc: &Document,
    manifest_file: FileId,
    diags: &mut Vec<Diagnostic>,
) {
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
        // 受け付ける道具は表が決める。表に1行足せば、ここも `tc.<名前>` の
        // 語彙も宣言の写しも揃って追随する。
        for (name, _, _) in dowel_eval::config::TOOLS {
            if let Some(e) = t.entry(name) {
                match e.value.as_str() {
                    Some(s) => decl.set_tool(name, s.to_string(), e.site),
                    None => type_err(diags, e.site, &format!("{label}.{name}"), "a string"),
                }
            }
        }
        // 取ってくる道具一式（ADR-0044）。`url` は `sha256` を要する——
        // URL は名前であり、名前の裏のバイトは変わりうる（ADR-0029 と同じ）。
        let url = t.entry("url").and_then(|e| e.value.as_str().map(|s| (s.to_string(), e.site)));
        let sha = t.entry("sha256").and_then(|e| e.value.as_str().map(|s| (s.to_string(), e.site)));
        match (url, sha) {
            (Some((url, site)), Some((sha256, sha_site))) => {
                if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                    diags.push(
                        Diagnostic::error(
                            "unpinned-toolchain",
                            format!("`sha256` of `{label}` is not 64 hexadecimal digits"),
                        )
                        .at(sha_site.file, sha_site.span, "this is not a sha256")
                        .note("the digest is of the archive itself, not of the unpacked tree"),
                    );
                } else {
                    decl.source = Some(ToolchainSource { url, sha256, site });
                }
            }
            (Some((_, site)), None) => diags.push(
                Diagnostic::error(
                    "unpinned-toolchain",
                    format!("`{label}` names a `url` but no `sha256`"),
                )
                .at(site.file, site.span, "declared here")
                .note("a URL is a name, and the bytes behind a name can change")
                .note("add `sha256 = \"...\"`, the digest of the archive"),
            ),
            // `sha256` だけは無害である。取りに行かない以上、何も検めない。
            (None, _) => {}
        }
        // sysroot（ADR-0047）。道具ではないので `TOOLS` の表には無い。
        if let Some(e) = t.entry("sysroot") {
            match e.value.as_str() {
                Some(s) => decl.sysroot = Some(s.to_string()),
                None => type_err(diags, e.site, &format!("{label}.sysroot"), "a string"),
            }
        }
        // 様式は道具ではない。名前ではなく綴り方を選ぶ宣言である（ADR-0027）。
        if let Some(e) = t.entry(STYLE_KEY) {
            match e.value.as_str().and_then(dowel_eval::config::Style::parse) {
                Some(style) => decl.style = Some(style),
                None => {
                    let mut d = Diagnostic::error(
                        "invalid-value",
                        format!("`{STYLE_KEY}` has to name an argument style"),
                    )
                    .at(e.site.file, e.site.span, "this is not a style")
                    .note(format!(
                        "the styles are: {}",
                        dowel_eval::config::Style::ALL.join(", ")
                    ))
                    .note("the style spells the arguments dowel assembles: `-I` vs `/I`, `-o` vs `/Fo:`")
                    .note("the flags you write yourself pass through untouched");
                    if let Some(name) = e.value.as_str() {
                        if let Some(c) = dowel_support::diag::closest(
                            name,
                            dowel_eval::config::Style::ALL.iter().copied(),
                        ) {
                            d = d.note(format!("did you mean `{c}`?"));
                        }
                    }
                    diags.push(d);
                }
            }
        }
        // 表に無いキーは拒む。黙って無視すると、道具の綴り間違いが既定値への
        // 無言の後退になる——クロスの archiver を打ち間違えると、ホストの
        // `ar` が黙って書庫を作る。#50 が防ごうとした状態が戻る（issue #59）
        let mut known: Vec<&str> = dowel_eval::config::TOOLS.iter().map(|(n, _, _)| *n).collect();
        known.push(STYLE_KEY);
        known.push("url");
        known.push("sha256");
        known.push("sysroot");
        for e in &t.entries {
            let name = e.key.join(".");
            if known.contains(&name.as_str()) {
                continue;
            }
            let mut d = Diagnostic::error("unknown-property", format!("unknown property `{name}`"))
                .at(e.site.file, e.site.span, "this key is not part of the toolchain")
                .note(format!("`[{label}]` accepts: {}", known.join(", ")));
            if let (Some(c), Some(&span)) = (
                dowel_support::diag::closest(&name, known.iter().copied()),
                e.key_spans.first().filter(|_| e.key.len() == 1),
            ) {
                d = d.suggest(e.site.file, span, c, format!("did you mean `{c}`?"));
            }
            diags.push(d);
        }
        match triple {
            Some(triple) => {
                // トリプル向けの宣言は、そのトリプルのビルド全体を担う。
                // `c` が無いと C のコンパイルとリンクがホストの既定へ落ち、
                // 成果物のアーキテクチャが黙って食い違う（issue #42）。
                if decl.tool("c").is_none() {
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
                // 既に在るものは道具ごとに残す。共有ファイルを土台に、
                // `dowel.toml` に書いた1つだけを差し替える形が要る。
                match pkg.toolchains.get_mut(&triple) {
                    Some(existing) => existing.fill_from(&decl),
                    None => {
                        pkg.toolchains.insert(triple, decl);
                    }
                }
            }
            None => pkg.toolchain.fill_from_or_replace(decl),
        }
    }
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

/// `sha256` が 64 桁の16進であることを確かめる。
///
/// git の `rev` と同じ要請である（[`pinned_rev`]）。書庫には rev に当たる
/// ものが無いので、**内容そのもの**で固定する——URL は同じ名前で別の中身を
/// 指しうるし、実際に差し替わる（[ADR-0029](../../../docs/adr/0029-tarball-dependencies.md)）。
fn declared_sha256(t: &dowel_eval::Table) -> Result<String, Option<String>> {
    let Some(e) = t.entry("sha256") else { return Err(None) };
    let Some(s) = e.value.as_str() else { return Err(Some(String::new())) };
    let hash = s.to_ascii_lowercase();
    if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(hash)
    } else {
        Err(Some(s.to_string()))
    }
}

/// 固定されていない書庫の依存を断る。
fn unhashed(
    diags: &mut Vec<Diagnostic>,
    file: dowel_support::FileId,
    t: &dowel_eval::Table,
    name: &str,
    found: Option<String>,
) {
    let span = t.entry("sha256").map(|e| e.site.span).unwrap_or(t.site.span);
    let d = match found {
        None => Diagnostic::error(
            "unpinned-dependency",
            format!("dependency `{name}` has a `url` but no `sha256`"),
        )
        .at(file, span, "an archive is pinned by its contents")
        .note("a URL can serve different bytes tomorrow; the hash is what makes the build the same one")
        .note("`dowel build` prints the hash it received, which can be pasted in once it has been checked"),
        Some(bad) => Diagnostic::error(
            "unpinned-dependency",
            format!("`{bad}` is not a sha256 digest"),
        )
        .at(file, span, "expected 64 hexadecimal digits")
        .note("this is the digest of the archive itself, not of its contents once unpacked"),
    };
    diags.push(d);
}

/// `[features]` の予約キー。機能名ではない。
pub const EXCLUSIVE: &str = "exclusive";

/// `exclusive = [["a", "b"], ...]` を読む。
///
/// 値は文字列の配列の配列である。1組は「同時に立ててはならない機能」であり、
/// 2つ以上の名前を要する——1つだけの組は何も禁じない。
fn read_exclusive(pkg: &mut Package, e: &dowel_eval::Entry, diags: &mut Vec<Diagnostic>) {
    let Some(groups) = e.value.as_list() else {
        type_err(diags, e.site, "features.exclusive", "an array of arrays of strings");
        return;
    };
    for group in groups {
        let Some(items) = group.as_list() else {
            type_err(diags, e.site, "features.exclusive", "an array of arrays of strings");
            continue;
        };
        let mut names = Vec::new();
        for item in items {
            match item.as_str() {
                Some(s) => names.push(s.to_string()),
                None => type_err(diags, e.site, "features.exclusive", "an array of strings"),
            }
        }
        if names.len() < 2 {
            diags.push(
                Diagnostic::warning(
                    "empty-exclusive-group",
                    "an exclusive group needs at least two features",
                )
                .at(pkg.manifest_file, e.site.span, "this group forbids nothing")
                .note("a group says which features cannot be enabled together"),
            );
            continue;
        }
        pkg.exclusive.push((names, e.site));
    }
}

/// 排他の組が挙げる名前が、この表で宣言されているか。
///
/// 表を全て読んでから確かめる。`exclusive` が先に書かれていることがある。
/// 宣言されていない名前を黙って受けると、その組は永久に成立せず、書いた人には
/// 「排他にしたはずのものが効かない」としか見えない。
fn check_exclusive_names(pkg: &mut Package, diags: &mut Vec<Diagnostic>) {
    let declared: Vec<String> = pkg.features.keys().filter(|k| *k != "default").cloned().collect();
    for (group, site) in &pkg.exclusive {
        for name in group {
            if pkg.features.contains_key(name) {
                continue;
            }
            let mut d = Diagnostic::error("unknown-feature", format!("unknown feature `{name}`"))
                .at(site.file, site.span, "named in an exclusive group")
                .note(format!(
                    "`[features]` declares: {}",
                    if declared.is_empty() { "(none)".to_string() } else { declared.join(", ") }
                ));
            if let Some(c) = dowel_support::diag::closest(name, declared.iter().map(|s| s.as_str()))
            {
                d = d.note(format!("did you mean `{c}`?"));
            }
            diags.push(d);
        }
    }
}

/// 依存の出所を名乗るキーと、その言い表し方。
///
/// 「1つだけ」という規則をこの表が持つ。出所を足すときはここに1行足す。
const SOURCE_KEYS: &[(&str, &str)] = &[
    ("path", "a local path"),
    ("git", "a git repository"),
    ("version", "a system package"),
    ("url", "an archive"),
];

/// 出所を2つ以上名乗っていないか。
///
/// 0個は `incomplete-dependency` で拒んでいる。2個を黙って受けるのは規則として
/// 片側しか無く、しかも黙って一方が勝つ。切り替えの途中——手元の `path` から
/// `git` へ移す、あるいは一時的に `path` へ差し替える——で消し忘れると、
/// その木を持たない誰かが組むまで気づかない（issue #79）。
fn conflicting_sources(t: &dowel_eval::Table, file: FileId, name: &str) -> Option<Diagnostic> {
    let present: Vec<_> = SOURCE_KEYS
        .iter()
        .filter_map(|(k, what)| t.entry(k).map(|e| (*k, *what, e.site)))
        .collect();
    if present.len() < 2 {
        return None;
    }
    let mut d = Diagnostic::error(
        "conflicting-dependency-source",
        format!("dependency `{name}` names more than one source"),
    );
    for (i, (_, what, site)) in present.iter().enumerate() {
        d = d.with_label(if i == 0 {
            Label::primary(file, site.span, *what)
        } else {
            Label::secondary(file, site.span, format!("and {what}"))
        });
    }
    Some(
        d.note(format!(
            "a dependency has exactly one source: {}",
            SOURCE_KEYS.iter().map(|(k, _)| format!("`{k}`")).collect::<Vec<_>>().join(", ")
        ))
        .note("only the first would be read; the others would never be fetched or resolved"),
    )
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
    resolve(pkg, requested, use_default).own
}

/// 1つのパッケージの機能解決の結果。
///
/// 機能名は2種類ある。素の名前はこのパッケージ自身の機能であり、
/// `dep/feat` は依存 `dep` の機能 `feat` を有効にする転送である
/// （[ADR-0017]）。転送は自分の集合には入らない——`feature.<名前>` は
/// 常に「このパッケージで有効か」を問うものであり、その値域は同じ
/// パッケージの `[features]` が決める。
///
/// [ADR-0017]: ../../../docs/adr/0017-feature-forwarding.md
#[derive(Default)]
pub struct Features {
    /// このパッケージで有効な機能
    pub own: std::collections::BTreeSet<String>,
    /// 依存の名前 → その依存で有効にする機能。宣言された位置つき
    pub forwarded: BTreeMap<String, Vec<(String, Site)>>,
}

/// 機能を解決し、自分の集合と依存への転送に分ける。
pub fn resolve(pkg: &Package, requested: &[String], use_default: bool) -> Features {
    let mut out = Features::default();
    let mut stack: Vec<String> = requested.to_vec();
    if use_default {
        if let Some(defaults) = pkg.features.get("default") {
            stack.extend(defaults.iter().cloned());
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    while let Some(f) = stack.pop() {
        if f == "default" || !seen.insert(f.clone()) {
            continue;
        }
        match f.split_once('/') {
            Some((dep, feat)) => {
                let site = pkg
                    .features_site
                    .unwrap_or(Site::new(pkg.manifest_file, dowel_support::Span::EMPTY));
                out.forwarded.entry(dep.to_string()).or_default().push((feat.to_string(), site));
            }
            None => {
                out.own.insert(f.clone());
                if let Some(enables) = pkg.features.get(&f) {
                    stack.extend(enables.iter().cloned());
                }
            }
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
            description: String::new(),
            id: PackageId(0),
            name: "p".into(),
            version: "0".into(),
            root: PathBuf::from("."),
            manifest_file: FileId(0),
            build_file: None,
            deps: Vec::new(),
            features: map,
            features_site: None,
            exclusive: Vec::new(),
            targets: Vec::new(),
            targets_site: None,
            toolchain: ToolchainDecl::default(),
            toolchains: BTreeMap::new(),
            toolchains_path: None,
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
        let site = Site::new(FileId(0), dowel_support::Span::new(0, 0));
        let mut p = pkg_with(&[]);
        p.toolchain.set_tool("c", "cc".into(), site);
        let mut cross = ToolchainDecl::default();
        cross.set_tool("c", "riscv64-gcc".into(), site);
        p.toolchains.insert(CROSS.into(), cross);

        // ホストのビルドは無印の宣言、別トリプルはそのトリプルの宣言。
        let command = |d: &ToolchainDecl| d.tool("c").map(|t| t.command.clone());
        assert_eq!(command(p.toolchain_for(HOST, true).unwrap()).as_deref(), Some("cc"));
        assert_eq!(command(p.toolchain_for(CROSS, false).unwrap()).as_deref(), Some("riscv64-gcc"));
    }

    #[test]
    fn a_foreign_triple_without_a_declaration_resolves_to_nothing() {
        let site = Site::new(FileId(0), dowel_support::Span::new(0, 0));
        let mut p = pkg_with(&[]);
        p.toolchain.set_tool("c", "cc".into(), site);
        // 無印の宣言はホスト向けであり、別トリプルのビルドへは落ちない。
        // ここが `Some` になると、ホストの成果物に別トリプルの名前が付く（issue #42）。
        assert!(p.toolchain_for("riscv64gc-unknown-linux-gnu", false).is_none());
    }

    #[test]
    fn terminates_on_cyclic_features() {
        let p = pkg_with(&[("a", &["b"]), ("b", &["a"])]);
        let f = resolve_features(&p, &["a".into()], false);
        assert_eq!(f.len(), 2);
    }
}
