//! マニフェストの読み込みとターゲットの構築。
//!
//! `Session` は「1回の CLI 実行が触れた全て」を保持する。読み込みの経路は
//! 増分クエリエンジン（[`crate::query`]）を通しており、
//! [`Session::reload`] は中身の変わらなかったファイルを解析し直さない。
//! プロセスを跨いだ再利用は永続化ストア（[`crate::persist`]）が担う。
//! 前回と本文が同じマニフェストは、解析も評価もせずに復元する。

use crate::package::{self, DepKind, Package};
use crate::persist::Cache;
use crate::query::{self, Key};
use crate::runner::Runner;
use crate::target::{label, ArtifactDecl, PackageId, PropMap, Target, TargetId};
use dowel_eval::schema::{self, Block, TableKind};
use dowel_eval::{Data, Document, Ns, Site, Value};
use dowel_query::{Db, Stats};
use dowel_store::Inputs;
use dowel_support::diag::closest;
use dowel_support::{log, Diagnostic, FileId, SourceMap, Span};
use dowel_support::{log_debug, log_trace};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

pub const MANIFEST_NAME: &str = "dowel.toml";
pub const BUILD_NAME: &str = "dowel.build";

/// `[<kind>.<name>.artifacts]` の見出し。プロパティのブロックではない
/// （issue #60）。
const ARTIFACTS_BLOCK: &str = "artifacts";

/// `[<kind>.<name>.inspect]` の見出し。同じく、プロパティのブロックではない。
/// 変換との違いは出力を持たないことだけである（issue #60）。
const INSPECT_BLOCK: &str = "inspect";

/// 読み込みの時点で分かっている機能フラグの選択。
///
/// 任意の依存を読むかどうかがこれで決まるため、構成（`Config`）より前に要る。
/// `Config` は根のマニフェストを読んだ後でなければ組み立てられない。
#[derive(Clone, Debug)]
pub struct Features {
    /// `--features` で明示された名前
    pub requested: Vec<String>,
    /// `default` を取り込むか（`--no-default-features` で偽）
    pub default: bool,
}

impl Default for Features {
    fn default() -> Features {
        Features { requested: Vec::new(), default: true }
    }
}

pub struct Session {
    pub sm: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
    pub packages: Vec<Package>,
    pub targets: Vec<Target>,
    /// ターゲットトリプル → 実行ラッパ（docs/30-devexp.md 1節）
    pub runners: BTreeMap<String, Runner>,
    /// 正規化したパッケージルート → 識別子。同じパッケージを2度読まないため。
    by_root: BTreeMap<PathBuf, PackageId>,
    /// 最初に読み込んだ根。`reload` の起点。
    root: PathBuf,
    /// 解析と評価のメモ表。`reload` を跨いで生き残る
    db: Db<Key>,
    /// 読み込んだファイルの記録。プロセスを跨いだ変更検出に使う
    inputs: Inputs,
    /// 前回の実行が残した入力の記録
    previous: Inputs,
    /// プロセスを跨いだ評価結果の再利用（ADR-0012）
    cache: Rc<Cache>,
    /// 任意の依存を読むかどうかの判定に使う選択
    features: Features,
    /// 根の `[features]` から解決した集合。根を読むまでは空
    active: std::collections::BTreeSet<String>,
    /// 値の入れ子の上限（`--max-nesting`）。既定は `dowel_syntax::MAX_NESTING`
    max_nesting: usize,
    /// エディタの緩衝。ここに在るパスはディスクより優先して読む。
    /// 保存されていない内容で解析するための経路（docs/20-architecture.md 6節）
    overlay: BTreeMap<PathBuf, String>,
    /// git 依存の取得を行うか。エディタからは打鍵ごとにネットワークへ
    /// 触れないよう、取得済みの checkout の再利用だけを行う
    fetch: bool,
    /// 外部依存の名前 → 合成パッケージ。pkg-config で解決したもの（ADR-0015）
    externals: BTreeMap<String, PackageId>,
}

impl Session {
    /// `root` にあるパッケージと、そこから到達する `path` 依存を読み込む。
    ///
    /// 機能フラグは既定（`default` を取り込み、明示の指定なし）で解決する。
    pub fn load(root: &Path) -> Session {
        Session::load_with(root, Features::default())
    }

    /// 機能フラグの選択を与えて読み込む。
    ///
    /// 有効でない任意の依存は読み込まない。読み込むと、実体を要求することになり、
    /// パッケージとしても依存グラフの節点として残る。取得を伴う供給形態
    /// （Phase 5）では、選ばれていない依存を取得することになる。
    pub fn load_with(root: &Path, features: Features) -> Session {
        Session::load_with_max_nesting(root, features, dowel_syntax::MAX_NESTING)
    }

    /// 値の入れ子の上限も与えて読み込む（`--max-nesting` の配管）。
    ///
    /// 上限は評価結果の指紋に混ざるため、上限を跨いだ再実行でストアが
    /// 古い結果を返すことはない（`query::fingerprint_of_source`）。
    pub fn load_with_max_nesting(root: &Path, features: Features, max_nesting: usize) -> Session {
        let mut sess = Session {
            sm: SourceMap::new(),
            diagnostics: Vec::new(),
            packages: Vec::new(),
            targets: Vec::new(),
            runners: BTreeMap::new(),
            by_root: BTreeMap::new(),
            root: canonical(root),
            db: Db::new(),
            inputs: Inputs::new(),
            previous: crate::persist::read_inputs(&canonical(root)),
            cache: Rc::new(Cache::open(&canonical(root))),
            features,
            active: std::collections::BTreeSet::new(),
            max_nesting,
            overlay: BTreeMap::new(),
            fetch: true,
            externals: BTreeMap::new(),
        };
        sess.walk();
        sess
    }

    /// エディタのために読み込む。
    ///
    /// 開いている緩衝（`overlay`）がディスクより優先され、正本になる。
    /// ストアは読みも書きもしない（未保存の内容から得た結果を書かないため —
    /// docs/20-architecture.md 6節）。git 依存の取得も行わず、取得済みの
    /// checkout の再利用に留める。打鍵ごとに作って捨てる前提であり、常駐しない。
    pub fn load_for_editor(root: &Path, overlay: BTreeMap<PathBuf, String>) -> Session {
        let overlay = overlay.into_iter().map(|(p, t)| (canonical(&p), t)).collect();
        let mut sess = Session {
            sm: SourceMap::new(),
            diagnostics: Vec::new(),
            packages: Vec::new(),
            targets: Vec::new(),
            runners: BTreeMap::new(),
            by_root: BTreeMap::new(),
            root: canonical(root),
            db: Db::new(),
            inputs: Inputs::new(),
            previous: Inputs::new(),
            cache: Rc::new(Cache::disabled()),
            features: Features::default(),
            active: std::collections::BTreeSet::new(),
            max_nesting: dowel_syntax::MAX_NESTING,
            overlay,
            fetch: false,
            externals: BTreeMap::new(),
        };
        sess.walk();
        sess
    }

    /// ファイルを読む。エディタの緩衝が在ればそちらが正本。
    fn read_source(&mut self, path: &Path) -> std::io::Result<FileId> {
        match self.overlay.get(path) {
            Some(text) => {
                let text = text.clone();
                Ok(self.sm.add(path, text))
            }
            None => self.sm.load(path),
        }
    }

    /// pkg-config で解決した外部依存を、合成パッケージとして繋ぐ。
    ///
    /// 実体はソースを持たない `lib` ターゲット1つで、`--cflags` / `--libs` を
    /// 公開の `flags` / `link_flags` として供給する。専用のノード種別を
    /// 設けないのは、伝播・併合・`why` の全経路をそのまま通すためである。
    /// 来歴は `pkg-config(...)` として現れる。
    fn insert_external(&mut self, name: &str, site: Site, r: &crate::pkgconfig::Resolved) {
        use dowel_eval::value::{Origin, Prov, Type};
        let pid = PackageId(self.packages.len());
        self.packages.push(Package {
            id: pid,
            name: name.to_string(),
            version: r.version.clone(),
            root: PathBuf::new(),
            manifest_file: site.file,
            build_file: None,
            deps: Vec::new(),
            features: BTreeMap::new(),
            features_site: None,
            toolchain: crate::package::ToolchainDecl::default(),
            toolchains: BTreeMap::new(),
        });
        let strs = |words: &[String]| {
            Value::list(
                Type::Str,
                words
                    .iter()
                    .map(|w| {
                        Value::str(w.clone(), Prov::at(Origin::Call("pkg-config".into()), site))
                    })
                    .collect(),
                Prov::at(Origin::Call("pkg-config".into()), site),
            )
        };
        let mut public = PropMap::new();
        if !r.cflags.is_empty() {
            public.insert("flags".to_string(), strs(&r.cflags));
        }
        if !r.libs.is_empty() {
            public.insert("link_flags".to_string(), strs(&r.libs));
        }
        let tid = TargetId(self.targets.len());
        self.targets.push(Target {
            id: tid,
            package: pid,
            kind: TableKind::Lib,
            name: name.to_string(),
            site,
            root: PropMap::new(),
            public,
            private: PropMap::new(),
            artifacts: Vec::new(),
            inspections: Vec::new(),
        });
        self.externals.insert(name.to_string(), pid);
        log_debug!("external dependency `{name}` {} via pkg-config", r.version);
    }

    /// 有効な機能フラグ。根の `[features]` から解決したもの。
    pub fn active_features(&self) -> &std::collections::BTreeSet<String> {
        &self.active
    }

    /// ディスクを読み直してモデルを組み直す。
    ///
    /// 中身が変わっていないファイルは字句解析・構文解析・評価をやり直さない。
    /// 監視モードと言語サーバの入口であり、増分がどれだけ効いたかは
    /// [`Session::query_stats`] で観測できる。
    pub fn reload(&mut self) {
        self.diagnostics.clear();
        self.packages.clear();
        self.targets.clear();
        self.runners.clear();
        self.by_root.clear();
        self.active.clear();
        self.externals.clear();
        self.db.reset_stats();
        self.walk();
    }

    /// 解析と評価の再利用状況。`hit` はメモをそのまま使った件数。
    pub fn query_stats(&self) -> Stats {
        self.db.stats()
    }

    /// 構成と解決済みの依存をクエリへ渡す。
    ///
    /// 依存の解決は `Session` の外（[`crate::graph::build`]）で行う。名前解決に
    /// 全パッケージが要るためであり、その段をクエリにするのは別の増分である。
    pub fn declare_derivations(&self, cfg: &dowel_eval::Config, graph: &crate::graph::Graph) {
        query::set_config(&self.db, cfg);
        for t in &self.targets {
            let deps =
                graph.deps_of(t.id).iter().map(|e| (self.label(e.to), e.block)).collect::<Vec<_>>();
            query::set_deps(&self.db, &self.label(t.id), deps);
        }
    }

    /// 依存側へ供給するプロパティ。メモを経由する。
    pub fn interface_of(&self, id: TargetId) -> Arc<query::Merged> {
        query::interface(&self.db, &self.label(id))
            .expect("the session never cancels its own queries")
    }

    /// 自身のコンパイルに効くプロパティ。メモを経由する。
    pub fn compile_env_of(&self, id: TargetId) -> Arc<query::Merged> {
        query::compile_env(&self.db, &self.label(id))
            .expect("the session never cancels its own queries")
    }

    /// 今回の実行が読んだ入力と、計算した評価結果をストアへ書く。
    ///
    /// 書けなくても誤りではない。次回の実行が計算し直すだけであり、
    /// 結果は変わらない。書き手を取得できない場合も同様である。
    pub fn save(&self) -> crate::persist::Saved {
        self.cache.save(&self.inputs)
    }

    /// 前回の実行から見た入力の変化。プロセスを跨いだ観測に使う。
    pub fn input_changes(&self) -> Vec<(PathBuf, dowel_store::input::Change)> {
        let mut out = Vec::new();
        for (path, _) in self.sm.paths() {
            let change = self.previous.check(&path, || {
                std::fs::read_to_string(&path).ok().map(|t| dowel_store::fingerprint(t.as_bytes()))
            });
            out.push((path, change));
        }
        out
    }

    fn walk(&mut self) {
        let _phase = log::Phase::start("load");
        // 2つ目の要素は、その位置を読ませた宣言の位置。根には無い。
        // 読めなかった場合の診断がこれを指す。
        let mut queue: Vec<(PathBuf, Option<Site>)> = vec![(self.root.clone(), None)];
        let mut root_seen = false;
        while let Some((dir, from)) = queue.pop() {
            if self.by_root.contains_key(&dir) {
                continue;
            }
            let Some(id) = self.load_package(&dir, from) else { continue };
            // 機能集合は根の `[features]` が決める。根を読んだ時点で確定する。
            if !root_seen {
                root_seen = true;
                self.active = package::resolve_features(
                    &self.packages[id.0],
                    &self.features.requested,
                    self.features.default,
                );
                log_debug!(
                    "active features: {}",
                    if self.active.is_empty() {
                        "(none)".to_string()
                    } else {
                        self.active.iter().cloned().collect::<Vec<_>>().join(", ")
                    }
                );
            }
            for dep in self.packages[id.0].deps.clone() {
                if !package::is_active(&dep, &self.active) {
                    log_debug!(
                        "not reading optional dependency `{}`; its feature is off",
                        dep.name
                    );
                    continue;
                }
                match &dep.kind {
                    DepKind::Path(rel) => {
                        queue.push((canonical(&dir.join(rel)), Some(dep.source_site)));
                    }
                    // 取得はここで行う。rev が固定されているため、2回目以降は
                    // ネットワークに触れない（crate::fetch のモジュール説明）。
                    DepKind::Git { url, rev } => {
                        if self.fetch {
                            match crate::fetch::ensure(
                                &self.root,
                                &dep.name,
                                url,
                                rev,
                                dep.source_site,
                            ) {
                                Ok(d) => queue.push((canonical(&d), Some(dep.source_site))),
                                Err(d) => self.diagnostics.push(*d),
                            }
                        } else if let Some(d) = crate::fetch::existing(&self.root, &dep.name, rev) {
                            // エディタからは取得しない。checkout が無ければ
                            // 静かに読み飛ばす（CLI が取得と診断を担う）。
                            queue.push((canonical(&d), Some(dep.source_site)));
                        }
                    }
                    // `version` 依存はシステムの pkg-config で解決する（ADR-0015）。
                    // 解決結果は合成パッケージとして繋ぎ、dowel.lock と突き合わせる。
                    // エディタからは外部プロセスを起動しない（git と同じ扱い）。
                    DepKind::PkgConfig { min_version } => {
                        if self.fetch {
                            match crate::pkgconfig::resolve(&dep.name, min_version, dep.source_site)
                            {
                                Ok(r) => {
                                    if let Some(d) = crate::lock::reconcile(
                                        &self.root,
                                        &dep.name,
                                        &r.version,
                                        "pkg-config",
                                        dep.source_site,
                                    ) {
                                        self.diagnostics.push(d);
                                    }
                                    self.insert_external(&dep.name, dep.source_site, &r);
                                }
                                Err(d) => self.diagnostics.push(*d),
                            }
                        }
                    }
                    DepKind::Unsupported(_) => {}
                }
            }
        }
        // 宣言をクエリへ渡す。指紋はスパンを含まない要約から導くため、
        // コメントだけの編集ではここで版が進まない。
        for t in &self.targets {
            let label = label(&self.packages[t.package.0].name, &t.name);
            query::set_declared(&self.db, &label, t.public.clone(), t.private.clone());
        }
        log_debug!("loaded {} packages and {} targets", self.packages.len(), self.targets.len());
        let s = self.db.stats();
        log_debug!(
            "queries: {} computed, {} reused, {} verified, {} skipped by durability",
            s.computed,
            s.hit,
            s.verified,
            s.skipped
        );
    }

    fn load_package(&mut self, dir: &Path, from: Option<Site>) -> Option<PackageId> {
        let manifest_path = dir.join(MANIFEST_NAME);
        let manifest_file = match self.read_source(&manifest_path) {
            Ok(f) => f,
            Err(e) => {
                let mut d = Diagnostic::error(
                    "missing-manifest",
                    format!("cannot read {}: {e}", manifest_path.display()),
                )
                .note("a package root requires a `dowel.toml`");
                // 根には宣言が無い。依存として辿り着いた場合だけ位置を持つ。
                if let Some(s) = from {
                    d = d.at(s.file, s.span, "this dependency does not name a package root");
                }
                self.diagnostics.push(d);
                return None;
            }
        };

        let id = PackageId(self.packages.len());
        let manifest = self.parse_and_eval(manifest_file, true);
        let mut diags = Vec::new();
        let mut pkg =
            package::from_document(id, &manifest.doc, dir.to_path_buf(), manifest_file, &mut diags);
        self.diagnostics.append(&mut diags);

        // 読めなかった `dowel.build` を指す位置は、それを要求している
        // `[package]` の宣言である。ファイル自体が無いためそこは指せない。
        let package_site = manifest
            .doc
            .table(&["package"])
            .map(|t| t.site)
            .unwrap_or(Site::new(manifest_file, Span::EMPTY));

        let build_path = dir.join(BUILD_NAME);
        if self.overlay.contains_key(&build_path) || build_path.exists() {
            match self.read_source(&build_path) {
                Ok(f) => {
                    pkg.build_file = Some(f);
                    let build = self.parse_and_eval(f, false);
                    self.by_root.insert(dir.to_path_buf(), id);
                    self.packages.push(pkg);
                    self.check_feature_refs(id, &build.doc);
                    self.build_targets(id, &build.doc);
                    log_debug!(
                        "loaded package `{}` from {}",
                        self.packages[id.0].name,
                        dir.display()
                    );
                    return Some(id);
                }
                Err(e) => self.diagnostics.push(
                    Diagnostic::error(
                        "unreadable-build",
                        format!("cannot read {}: {e}", build_path.display()),
                    )
                    .at(package_site.file, package_site.span, "declared here"),
                ),
            }
        } else {
            self.diagnostics.push(
                Diagnostic::error("missing-build", format!("missing {}", build_path.display()))
                    .at(package_site.file, package_site.span, "this package has no targets")
                    .note("target definitions belong in `dowel.build` (docs/10-manifest.md)"),
            );
        }

        self.by_root.insert(dir.to_path_buf(), id);
        self.packages.push(pkg);
        Some(id)
    }

    /// ファイルを解析して評価する。結果はクエリエンジンのメモに残り、
    /// `reload` で中身が変わっていなければそのまま再利用される。
    fn parse_and_eval(&mut self, file: FileId, strict: bool) -> Arc<query::Evaluated> {
        let src = self.sm.text(file).to_string();
        log_debug!(
            "reading {} ({} bytes, strict={strict})",
            self.sm.path(file).display(),
            src.len()
        );
        // クエリのログは鍵（`FileId`）でしか語れない。突き合わせられるよう
        // ここで対応を出しておく。
        log_trace!("  file {} is {}", file.0, self.sm.path(file).display());
        query::set_text(&self.db, file, &src);
        // プロセスを跨いだ変更検出のために記録する。プロセス内の判定は
        // クエリエンジンが行うため、ここでは記録だけを行う。
        self.inputs.record(self.sm.path(file), dowel_store::fingerprint(src.as_bytes()));
        // `Session` は打ち切りを公開していないため、この `Db` が
        // 打ち切られることはない。言語サーバを載せる際は、
        // ここが `Result` の伝播点になる。
        let out =
            query::evaluated(&self.db, file, strict, self.max_nesting, Some(self.cache.clone()))
                .expect("the session never cancels its own queries");
        self.diagnostics.extend(out.diagnostics.iter().cloned());
        out
    }

    /// `dowel.build` が参照する機能名を、`dowel.toml` の宣言に照らす。
    ///
    /// 評価の段では判定できない。`feature.` の値域は同じパッケージの
    /// `dowel.toml` が決めるものであり、`dowel.build` を1ファイルとして
    /// 評価する時点では手元にない。
    ///
    /// 宣言されていない名前は偽と評価されるため、綴りを誤った分岐は
    /// 「無効にした機能」と区別が付かない。
    fn check_feature_refs(&mut self, pkg: PackageId, doc: &Document) {
        let declared: Vec<String> = self.packages[pkg.0].features.keys().cloned().collect();
        let mut diags = Vec::new();
        for r in &doc.cfg_refs {
            if r.key.ns != Ns::Feature || declared.contains(&r.key.name) {
                continue;
            }
            diags.push(unknown_feature(
                &r.key.name,
                &declared,
                Some(r.site),
                "this feature is not declared in `dowel.toml`",
            ));
        }
        self.diagnostics.append(&mut diags);
    }

    /// `dowel.build` の各テーブルをターゲットへ組み上げる。
    fn build_targets(&mut self, pkg: PackageId, doc: &Document) {
        TargetSink {
            pkg,
            targets: &mut self.targets,
            runners: &mut self.runners,
            diagnostics: &mut self.diagnostics,
        }
        .build(doc);
    }
}

/// `dowel.build` のテーブル列をターゲットとランナーへ組み上げる先。
///
/// [`Session`] の読み込みと、開いている1ファイルだけを見る検査
/// （[`check_build_file`]、issue #38）が同じ実装を共有する。分けて持つと、
/// 片方だけを直したときに CLI とエディタの診断が黙って食い違う。
struct TargetSink<'a> {
    pkg: PackageId,
    targets: &'a mut Vec<Target>,
    runners: &'a mut BTreeMap<String, Runner>,
    diagnostics: &'a mut Vec<Diagnostic>,
}

/// 開いている1ファイルの `dowel.build` を型検査する。
///
/// ワークスペースの模型もディスクも要らない。言語サーバが「開いている
/// 1ファイルで決まる」検査を `dowel check` と同じ実装で出すための入口である
/// （issue #38）。ファイルを跨ぐ検査（機能名の照合、依存の解決、併合）と
/// 計画段の検査（パス解決、glob 展開）はここには無い。
pub fn check_build_file(doc: &Document) -> Vec<Diagnostic> {
    let mut targets = Vec::new();
    let mut runners = BTreeMap::new();
    let mut diagnostics = Vec::new();
    TargetSink {
        pkg: PackageId(0),
        targets: &mut targets,
        runners: &mut runners,
        diagnostics: &mut diagnostics,
    }
    .build(doc);
    diagnostics
}

/// 開いている1ファイルの `dowel.toml` を型検査する。
///
/// [`package::from_document`] の診断だけを取り出す。読み取り自体が
/// ディスクに触れないため、そのまま言語サーバから使える。
pub fn check_manifest_file(doc: &Document, file: FileId) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let _ = package::from_document(PackageId(0), doc, PathBuf::from("."), file, &mut diagnostics);
    diagnostics
}

impl TargetSink<'_> {
    fn build(&mut self, doc: &Document) {
        let pkg = self.pkg;
        // `[lib.foo]` と `[lib.foo.public]` は別テーブルだが同じターゲットを指す。
        let mut index: BTreeMap<(String, String), TargetId> = BTreeMap::new();

        for table in &doc.tables {
            if table.path.is_empty() {
                if let Some(first) = table.entries.first() {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "toplevel-entry",
                            "a key cannot appear at the top level of `dowel.build`",
                        )
                        .at(first.site.file, first.site.span, "write it inside a table header")
                        .note("declare a target first, as in `[lib.<name>]`"),
                    );
                }
                continue;
            }

            let Some(kind) = self.parse_kind(table, doc.file) else { continue };
            // ランナーはターゲットではない。成果物を生成せず、伝播もしない。
            if kind == TableKind::Runner {
                self.declare_runner(table, doc.file);
                continue;
            }
            if table.path.len() < 2 {
                self.diagnostics.push(
                    Diagnostic::error(
                        "missing-target-name",
                        format!("`[{}]` has no target name", table.path.join(".")),
                    )
                    .at(
                        doc.file,
                        table.site.span,
                        format!("write `[{}.<name>]`", kind.name()),
                    ),
                );
                continue;
            }
            let name = table.path[1].clone();

            let key = (kind.name().to_string(), name.clone());
            // `artifacts` はプロパティのブロックではない。伝播の範囲を分ける
            // `public` / `private` と違い、成果物から成果物を作る宣言である
            // （issue #60）。ターゲットは先に作っておく必要がある。
            let is_artifacts = table.path.len() == 3 && table.path[2] == ARTIFACTS_BLOCK;
            let is_inspect = table.path.len() == 3 && table.path[2] == INSPECT_BLOCK;

            let block = match table.path.len() {
                2 => Block::Root,
                3 if is_artifacts || is_inspect => Block::Root,
                3 => match Block::parse(&table.path[2]) {
                    Some(b) => b,
                    None => {
                        let mut d = Diagnostic::error(
                            "unknown-block",
                            format!("unknown block `{}`", table.path[2]),
                        )
                        .at(
                            doc.file,
                            table.site.span,
                            "only `public`, `private`, `artifacts`, or `inspect`",
                        )
                        .note("propagating and non-propagating properties are separated syntactically (docs/10-manifest.md)");
                        if let (Some(c), Some(&span)) = (
                            closest(
                                &table.path[2],
                                ["public", "private", ARTIFACTS_BLOCK, INSPECT_BLOCK],
                            ),
                            table.path_spans.get(2),
                        ) {
                            d = d.suggest(doc.file, span, c, format!("did you mean `{c}`?"));
                        }
                        self.diagnostics.push(d);
                        continue;
                    }
                },
                _ => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "too-deep-table",
                            format!("`[{}]` is nested too deeply", table.path.join(".")),
                        )
                        .at(
                            doc.file,
                            table.site.span,
                            "expected `[kind.name]` or `[kind.name.block]`",
                        ),
                    );
                    continue;
                }
            };

            let tid = *index.entry(key).or_insert_with(|| {
                let tid = TargetId(self.targets.len());
                self.targets.push(Target {
                    id: tid,
                    package: pkg,
                    kind,
                    name: name.clone(),
                    site: table.site,
                    root: PropMap::new(),
                    public: PropMap::new(),
                    private: PropMap::new(),
                    artifacts: Vec::new(),
                    inspections: Vec::new(),
                });
                tid
            });

            if is_artifacts || is_inspect {
                self.declare_tool_runs(tid, table, is_artifacts);
                continue;
            }

            for entry in &table.entries {
                self.assign_prop(
                    tid,
                    block,
                    entry.key.clone(),
                    &entry.key_spans,
                    entry.value.clone(),
                    entry.site,
                );
            }
        }

        for t in self.targets.iter() {
            if t.package == pkg {
                log_trace!("declared target {}.{}", t.kind.name(), t.name);
            }
        }
    }

    /// `[<kind>.<name>.artifacts]` / `[<kind>.<name>.inspect]` を取り込む
    /// （issue #60）。
    ///
    /// 各項目はインラインテーブルであり、変換なら鍵が出力の拡張子、検査なら
    /// 表示に使う名前になる。`tool` は宣言できる道具
    /// （`dowel_eval::config::TOOLS`）の名前でなければならない。実体の名前
    /// （`arm-none-eabi-objcopy`）を直に書かせないのは、それを書くと
    /// トリプルごとの選択も記録された入力も効かなくなるためである。
    ///
    /// 2つのブロックが同じ読み取りを共有するのは、宣言の形が同じだからで
    /// ある。違いは出力を持つかどうかだけで、それは置かれたブロックが決める。
    fn declare_tool_runs(&mut self, tid: TargetId, table: &dowel_eval::Table, transform: bool) {
        let known = if transform { schema::artifact_props() } else { schema::inspection_props() };
        let what = if transform { "an artifact" } else { "an inspection" };
        for entry in &table.entries {
            let suffix = entry.key.join(".");
            let Data::Map(fields) = &entry.value.data else {
                self.diagnostics.push(
                    Diagnostic::error(
                        "type-mismatch",
                        format!(
                            "`{suffix}` is an inline table but {} was given",
                            entry.value.ty.display()
                        ),
                    )
                    .at(entry.site.file, entry.site.span, "expected `{ tool = \"...\", ... }`")
                    .note("write for example `bin = { tool = \"objcopy\", args = [\"-O\", \"binary\"] }`"),
                );
                continue;
            };

            let names: Vec<&str> = known.iter().map(|p| p.name).collect();
            for (name, value) in fields {
                match known.iter().find(|p| p.name == name) {
                    Some(def) if !def.ty.accepts(&value.ty) => self.diagnostics.push(
                        Diagnostic::error(
                            "type-mismatch",
                            format!(
                                "`{name}` is {} but {} was given",
                                def.ty.display(),
                                value.ty.display()
                            ),
                        )
                        .at(
                            entry.site.file,
                            entry.site.span,
                            format!("this value has type {}", value.ty.display()),
                        ),
                    ),
                    Some(_) => {}
                    None => {
                        let mut d = Diagnostic::error(
                            "unknown-property",
                            format!("unknown property `{name}`"),
                        )
                        .at(
                            entry.site.file,
                            entry.site.span,
                            format!("{what} has no such property"),
                        )
                        .note(format!("{what} accepts: {}", names.join(", ")));
                        if let Some(c) = closest(name, names.iter().copied()) {
                            d = d.note(format!("did you mean `{c}`?"));
                        }
                        self.diagnostics.push(d);
                    }
                }
            }

            let Some(tool) = fields.get("tool") else {
                self.diagnostics.push(
                    Diagnostic::error("missing-field", format!("`{suffix}` has no `tool`"))
                        .at(entry.site.file, entry.site.span, "write `tool = \"...\"`")
                        .note(format!(
                            "declarable tools: {}",
                            dowel_eval::config::TOOLS
                                .iter()
                                .map(|(n, _)| *n)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                );
                continue;
            };
            let Some(tool_name) = tool.as_str() else { continue };

            // 道具の名前は表が決める。実体の選択は `[toolchain]` の仕事。
            let tools: Vec<&str> = dowel_eval::config::TOOLS.iter().map(|(n, _)| *n).collect();
            if !tools.contains(&tool_name) {
                let mut d = Diagnostic::error(
                    "unknown-tool",
                    format!("`{tool_name}` is not a toolchain tool"),
                )
                .at(entry.site.file, entry.site.span, "no such tool")
                .note(format!("declarable tools: {}", tools.join(", ")))
                .note("the concrete command comes from `[toolchain]`, so write the tool's name here, not `arm-none-eabi-objcopy`");
                if let Some(c) = closest(tool_name, tools.iter().copied()) {
                    d = d.note(format!("did you mean `{c}`?"));
                }
                self.diagnostics.push(d);
                continue;
            }

            let decl = ArtifactDecl {
                suffix,
                tool: tool_name.to_string(),
                args: fields.get("args").cloned(),
                site: entry.site,
                tool_site: entry.site,
            };
            let target = &mut self.targets[tid.0];
            if transform {
                target.artifacts.push(decl);
            } else {
                target.inspections.push(decl);
            }
        }
    }

    /// `[runner.<triple>]` を取り込む。
    ///
    /// ランナーは構成（ターゲットトリプル）に紐づくものであり、パッケージには
    /// 紐づかない。複数のパッケージが同じトリプルに宣言した場合は、最初に
    /// 読んだものを採る。上書きを許すと「どのパッケージを根に置いたか」で
    /// テストの起動方法が変わり、再現しない。
    fn declare_runner(&mut self, table: &dowel_eval::Table, file: FileId) {
        if table.path.len() != 2 {
            self.diagnostics.push(
                Diagnostic::error(
                    "missing-target-name",
                    format!("`[{}]` has no target triple", table.path.join(".")),
                )
                .at(file, table.site.span, "write `[runner.<triple>]`")
                .note("a runner is selected by the target triple (docs/30-devexp.md)"),
            );
            return;
        }
        let triple = table.path[1].clone();

        let mut props = PropMap::new();
        let known = schema::runner_props();
        for entry in &table.entries {
            let name = entry.key.join(".");
            let Some(def) = known.iter().find(|p| p.name == name) else {
                let names: Vec<&str> = known.iter().map(|p| p.name).collect();
                let mut d =
                    Diagnostic::error("unknown-property", format!("unknown property `{name}`"))
                        .at(
                            entry.site.file,
                            entry.site.span,
                            "`runner` has no property with this name",
                        )
                        .note(format!("`runner` accepts: {}", names.join(", ")));
                // 提案は誤った鍵だけを置き換える。ドット付きの鍵は
                // `closest` に一致しないため、単一の段のときだけ提案する。
                if let (Some(c), Some(&span)) = (
                    closest(&name, names),
                    entry.key_spans.first().filter(|_| entry.key.len() == 1),
                ) {
                    d = d.suggest(entry.site.file, span, c, format!("did you mean `{c}`?"));
                }
                self.diagnostics.push(d);
                continue;
            };
            if !def.ty.accepts(&entry.value.ty) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "type-mismatch",
                        format!(
                            "`{name}` is {} but {} was given",
                            def.ty.display(),
                            entry.value.ty.display()
                        ),
                    )
                    .at(
                        entry.site.file,
                        entry.site.span,
                        format!("this value has type {}", entry.value.ty.display()),
                    ),
                );
                continue;
            }
            props.insert(name, entry.value.clone());
        }

        // 転送は「どこへ置くか」と「どう運ぶか」の両方が要る。片方だけでは
        // 成果物の置き場が決まらないか、置き場だけあって運ぶ手段が無い。
        let has_transfer = props.contains_key("transfer");
        let has_remote_dir = props.contains_key("remote_dir");
        if has_transfer != has_remote_dir {
            let (present, absent) =
                if has_transfer { ("transfer", "remote_dir") } else { ("remote_dir", "transfer") };
            self.diagnostics.push(
                Diagnostic::error(
                    "incomplete-runner",
                    format!("runner `{triple}` sets `{present}` but not `{absent}`"),
                )
                .at(file, table.site.span, "both are required to transfer the artifact")
                .note("without `remote_dir` there is no destination path")
                .note("without `transfer` there is no way to move the artifact"),
            );
            return;
        }
        if props.contains_key("host") && !has_transfer {
            self.diagnostics.push(
                Diagnostic::error(
                    "incomplete-runner",
                    format!("runner `{triple}` sets `host` but does not transfer anything"),
                )
                .at(file, table.site.span, "`host` only shapes the transfer destination")
                .note("set `transfer` and `remote_dir`, or remove `host`"),
            );
            return;
        }

        if !props.contains_key("command") {
            self.diagnostics.push(
                Diagnostic::error("missing-field", format!("runner `{triple}` has no `command`"))
                    .at(file, table.site.span, "a runner must say what to launch")
                    .note("for example `command = \"qemu-riscv64\"`"),
            );
            return;
        }

        if let Some(prev) = self.runners.get(&triple) {
            let mut d = Diagnostic::error(
                "duplicate-table",
                format!("runner `{triple}` is declared twice"),
            )
            .at(file, table.site.span, "declared again here")
            .note("a runner belongs to the target triple, not to a package");
            d = d.with_label(dowel_support::Label::secondary(
                prev.site.file,
                prev.site.span,
                "first declared here",
            ));
            self.diagnostics.push(d);
            return;
        }

        log_debug!("declared runner for `{triple}`");
        for (k, v) in &props {
            log_trace!("  {k} = {}", v.display());
        }
        self.runners.insert(triple.clone(), Runner { triple, site: table.site, props });
    }

    fn parse_kind(&mut self, table: &dowel_eval::Table, file: FileId) -> Option<TableKind> {
        let head = &table.path[0];
        let Some(kind) = TableKind::parse(head) else {
            let known: Vec<&str> = TableKind::ALL.iter().map(|k| k.name()).collect();
            let mut d = Diagnostic::error("unknown-kind", format!("unknown table kind `{head}`"))
                .at(file, table.site.span, "no such kind")
                .note(format!("available kinds: {}", known.join(", ")));
            if let (Some(c), Some(&span)) = (closest(head, known), table.path_spans.first()) {
                d = d.suggest(file, span, c, format!("did you mean `{c}`?"));
            }
            self.diagnostics.push(d);
            return None;
        };
        if !kind.is_implemented() {
            self.diagnostics.push(
                Diagnostic::error(
                    "unimplemented-kind",
                    format!("`{}` is not implemented yet", kind.name()),
                )
                .at(file, table.site.span, "recognized as a kind but not yet processed")
                .note("implemented kinds are lib, bin and test"),
            );
            return None;
        }
        Some(kind)
    }

    /// スキーマに照らしてプロパティを検査し、ターゲットへ格納する。
    fn assign_prop(
        &mut self,
        tid: TargetId,
        block: Block,
        key: Vec<String>,
        key_spans: &[Span],
        value: Value,
        site: Site,
    ) {
        // `[lib.foo]` の中に `public.includes = ...` と書く形も許す。
        // 3つ目の要素は、名前が書かれた段の添字である。名前が段をまたぐ
        // 場合（`a.b.c` など）は該当する段がないため `None` になる。
        let (block, name, at) = match key.len() {
            1 => (block, key[0].clone(), Some(0)),
            2 if block == Block::Root => match Block::parse(&key[0]) {
                Some(b) => (b, key[1].clone(), Some(1)),
                None => (block, key.join("."), None),
            },
            _ => (block, key.join("."), None),
        };

        let Some(def) = schema::lookup(block, &name) else {
            let known = schema::prop_names(block);
            let mut d = Diagnostic::error("unknown-property", format!("unknown property `{name}`"))
                .at(
                    site.file,
                    site.span,
                    format!("`{}` has no property with this name", block.name()),
                )
                .note(format!("`{}` accepts: {}", block.name(), known.join(", ")));
            // 範囲は誤った鍵だけを覆う。key-value 全体を覆うと、適用した結果から
            // 値が消える（#12）。ラベルの範囲は「どの記述が誤りか」を示すため
            // 行全体のままにする。
            let span = at.and_then(|i| key_spans.get(i)).copied();
            if let (Some(c), Some(span)) = (closest(&name, known.iter().copied()), span) {
                d = d.suggest(site.file, span, c, format!("did you mean `{c}`?"));
            } else if let Some(other) = other_block_with(&name, block) {
                d = d.note(format!("`{name}` is a property of `{}`", other.name()));
            }
            self.diagnostics.push(d);
            return;
        };

        if !def.ty.accepts(&value.ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "type-mismatch",
                    format!(
                        "`{name}` is {} but {} was given",
                        def.ty.display(),
                        value.ty.display()
                    ),
                )
                .at(site.file, site.span, format!("this value has type {}", value.ty.display()))
                .note(path_hint(&def.ty, &value.ty)),
            );
            return;
        }

        // 閉じた語彙を持つプロパティは、値そのものも検査する。型が `Str` で
        // あることだけでは、`c++2a` のような綴りが素通りして `-std=c++2a` が
        // コンパイラへ渡り、誤りがコンパイラの言葉で返ってくる。
        if let Some(domain) = def.domain {
            for leaf in str_leaves(&value) {
                let Some(text) = leaf.as_str() else { continue };
                if domain.contains(&text) {
                    continue;
                }
                let mut d = Diagnostic::error(
                    "unknown-standard",
                    format!("`{text}` is not a value of `{name}`"),
                )
                .at(
                    leaf.prov.nearest_site().map(|s| s.file).unwrap_or(site.file),
                    leaf.prov.nearest_site().map(|s| s.span).unwrap_or(site.span),
                    "not a known language standard",
                )
                .note(format!("`{name}` accepts: {}", domain.join(", ")));
                if let Some(c) = closest(text, domain.iter().copied()) {
                    d = d.note(format!("did you mean `{c}`?"));
                }
                self.diagnostics.push(d);
                return;
            }
        }

        let target = &mut self.targets[tid.0];
        if let Some(prev) = target.props(block).get(&name) {
            let prev_site = prev.prov.nearest_site();
            let mut d = Diagnostic::error(
                "duplicate-property",
                format!("`{name}` is set twice in the same block"),
            )
            .at(site.file, site.span, "set again here");
            if let Some(s) = prev_site {
                d = d.with_label(dowel_support::Label::secondary(s.file, s.span, "first set here"));
            }
            self.diagnostics.push(d);
            return;
        }
        log_trace!(
            "  {}.{} {} = {}",
            self.targets[tid.0].name,
            block.name(),
            name,
            value.display()
        );
        self.targets[tid.0].props_mut(block).insert(name, value);
    }
}

impl Session {
    pub fn package(&self, id: PackageId) -> &Package {
        &self.packages[id.0]
    }

    /// ソースファイルの属するパッケージ。
    ///
    /// 伝播した `Path` の基準点を決めるために要る。値は「パッケージルートからの
    /// 相対」で表されるが、どのパッケージかは値自身ではなく宣言された位置が持つ。
    pub fn package_of_file(&self, file: FileId) -> Option<PackageId> {
        self.packages
            .iter()
            .find(|p| p.manifest_file == file || p.build_file == Some(file))
            .map(|p| p.id)
    }

    /// 依存名から読み込み済みパッケージを引く。
    /// レジストリ依存は取得が未実装のため `None` になる。
    pub fn dep_package(&self, from: PackageId, dep_name: &str) -> Option<PackageId> {
        let pkg = self.package(from);
        let dep = pkg.deps.iter().find(|d| d.name == dep_name)?;
        match &dep.kind {
            DepKind::Path(rel) => self.by_root.get(&canonical(&pkg.root.join(rel))).copied(),
            DepKind::Git { rev, .. } => self
                .by_root
                .get(&canonical(&crate::fetch::checkout_dir(&self.root, &dep.name, rev)))
                .copied(),
            DepKind::PkgConfig { .. } => self.externals.get(dep_name).copied(),
            DepKind::Unsupported(_) => None,
        }
    }

    pub fn target(&self, id: TargetId) -> &Target {
        &self.targets[id.0]
    }

    pub fn label(&self, id: TargetId) -> String {
        let t = self.target(id);
        label(&self.package(t.package).name, &t.name)
    }

    /// `pkg:name` または `name` でターゲットを引く。
    /// 名前のみの指定は、一意に定まる場合に限り受け付ける。
    pub fn find_target(&self, spec: &str) -> Result<TargetId, String> {
        if let Some((pkg, name)) = spec.split_once(':') {
            return self
                .targets
                .iter()
                .find(|t| t.name == name && self.package(t.package).name == pkg)
                .map(|t| t.id)
                .ok_or_else(|| format!("no target named `{spec}`"));
        }
        let matches: Vec<&Target> = self.targets.iter().filter(|t| t.name == spec).collect();
        match matches.len() {
            0 => {
                let all: Vec<String> = self.targets.iter().map(|t| self.label(t.id)).collect();
                Err(format!(
                    "no target named `{spec}`. available: {}",
                    if all.is_empty() { "(none)".to_string() } else { all.join(", ") }
                ))
            }
            1 => Ok(matches[0].id),
            _ => {
                let labels: Vec<String> = matches.iter().map(|t| self.label(t.id)).collect();
                Err(format!(
                    "`{spec}` exists in several packages: {}. write `<package>:{spec}`",
                    labels.join(", ")
                ))
            }
        }
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == dowel_support::Severity::Error)
    }

    /// 根のパッケージ。最初に読み込んだもの。
    pub fn root_package(&self) -> Option<&Package> {
        self.packages.first()
    }
}

/// 宣言されていない機能名の診断。
///
/// `dowel.build` からの参照と `--features` の双方が使う。どちらも
/// 「`[features]` に無い名前」であり、注記と候補提示は同じものになる。
pub fn unknown_feature(
    name: &str,
    declared: &[String],
    at: Option<Site>,
    label: &str,
) -> Diagnostic {
    let listed: Vec<&str> =
        declared.iter().map(|s| s.as_str()).filter(|s| *s != "default").collect();
    let mut d =
        Diagnostic::error("unknown-feature", format!("unknown feature `{name}`")).note(format!(
            "`[features]` declares: {}",
            if listed.is_empty() { "(none)".to_string() } else { listed.join(", ") }
        ));
    if let Some(s) = at {
        d = d.at(s.file, s.span, label);
    }
    if let Some(c) = closest(name, listed) {
        match at {
            // 位置は `feature.<名前>` の全体を指す。置換もその形で書く。
            Some(s) => {
                d = d.suggest(
                    s.file,
                    s.span,
                    format!("feature.{c}"),
                    format!("did you mean `{c}`?"),
                )
            }
            None => d = d.note(format!("did you mean `{c}`?")),
        }
    }
    d
}

/// 同じ名前が別ブロックに存在するか。診断の注記に使う。
/// 値の中に現れる文字列の葉を全て集める。
///
/// 語彙の検査は具体化の前に行う。`match` の腕も後置 `when` の中身も、
/// どれか1つは選ばれうる以上、書かれた時点で確かめられる——選ばれるまで
/// 黙っていると、`--config=release` にした人だけが綴りの誤りに出会う。
fn str_leaves(value: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        match &v.data {
            Data::Str(_) => out.push(v),
            Data::List(items) => stack.extend(items),
            Data::Map(m) => stack.extend(m.values()),
            Data::Match { arms, .. } => stack.extend(arms.iter().map(|a| &a.value)),
            Data::When { inner, .. } => stack.push(inner),
            _ => {}
        }
    }
    out
}

fn other_block_with(name: &str, block: Block) -> Option<Block> {
    [Block::Root, Block::Public, Block::Private]
        .into_iter()
        .find(|&b| b != block && schema::lookup(b, name).is_some())
}

/// 型不一致のうち、頻出するものに具体的な助言を返す。
fn path_hint(expected: &dowel_eval::Type, actual: &dowel_eval::Type) -> String {
    use dowel_eval::Type;
    let wants_path = matches!(expected.elem().unwrap_or(expected), Type::Path);
    let gives_str = matches!(actual.elem().unwrap_or(actual), Type::Str);
    if wants_path && gives_str {
        "a Path is not built from a string; use `dir(\"...\")`, `file(\"...\")` or `glob(\"...\")`"
            .into()
    } else {
        format!("expected type {}", expected.display())
    }
}

/// 正規化に失敗しても落とさない。存在しないパスを指す診断は後段で出す。
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| lexical(p))
}

/// 実在しないパスの字句的な正規化。`.` と `..` を畳む。
///
/// エディタの緩衝は保存前の（ディスクに無い）パスを持ちうる。畳まないと
/// `dir/../lib` と `lib` が別の鍵になり、緩衝の重ね合わせが一致しない。
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// 位置を持たない診断のための空スパン。
pub const NO_SPAN: Span = Span::EMPTY;
