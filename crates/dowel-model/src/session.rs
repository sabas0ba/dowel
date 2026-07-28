//! マニフェストの読み込みとターゲットの構築。
//!
//! `Session` は「1回の CLI 実行が触れた全て」を保持する。読み込みの経路は
//! 増分クエリエンジン（[`crate::query`]）を通しており、
//! [`Session::reload`] は中身の変わらなかったファイルを解析し直さない。
//! 永続化ストア（docs/20-architecture.md 5節）を差し込む先も同じ場所であり、
//! `Db` のメモ表がその差し替え対象になる。

use crate::package::{self, DepKind, Package};
use crate::query::{self, Key};
use crate::runner::Runner;
use crate::target::{label, PackageId, PropMap, Target, TargetId};
use dowel_eval::schema::{self, Block, TableKind};
use dowel_eval::{Document, Site, Value};
use dowel_query::{Db, Stats};
use dowel_store::{Inputs, Store};
use dowel_support::diag::closest;
use dowel_support::{log, Diagnostic, FileId, SourceMap, Span};
use dowel_support::{log_debug, log_trace};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 入力の記録を置くファイル名。ストアのディレクトリ内に置く。
const INPUTS: &str = "inputs";

pub const MANIFEST_NAME: &str = "dowel.toml";
pub const BUILD_NAME: &str = "dowel.build";

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
    /// 任意の依存を読むかどうかの判定に使う選択
    features: Features,
    /// 根の `[features]` から解決した集合。根を読むまでは空
    active: std::collections::BTreeSet<String>,
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
            previous: read_inputs(&canonical(root)),
            features,
            active: std::collections::BTreeSet::new(),
        };
        sess.walk();
        sess
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
        self.db.reset_stats();
        self.walk();
    }

    /// 解析と評価の再利用状況。`hit` はメモをそのまま使った件数。
    pub fn query_stats(&self) -> Stats {
        self.db.stats()
    }

    /// 今回読み込んだ入力の記録をストアへ書く。
    ///
    /// 書けなくても誤りではない。次回の実行が変更検出をやり直すだけであり、
    /// 結果は変わらない。書き手を取得できない場合も同様である。
    pub fn save_inputs(&self) {
        let store = Store::open(&self.root);
        let Ok(Some(_writer)) = store.writer() else {
            log_debug!("inputs: not writing (no write lock)");
            return;
        };
        let dir = Store::dir(&self.root);
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join(INPUTS), self.inputs.encode());
            log_debug!("inputs: recorded {} files", self.inputs.len());
        }
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
        let mut queue = vec![self.root.clone()];
        let mut root_seen = false;
        while let Some(dir) = queue.pop() {
            if self.by_root.contains_key(&dir) {
                continue;
            }
            let Some(id) = self.load_package(&dir) else { continue };
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
                if let DepKind::Path(rel) = &dep.kind {
                    queue.push(canonical(&dir.join(rel)));
                }
            }
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

    fn load_package(&mut self, dir: &Path) -> Option<PackageId> {
        let manifest_path = dir.join(MANIFEST_NAME);
        let manifest_file = match self.sm.load(&manifest_path) {
            Ok(f) => f,
            Err(e) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "missing-manifest",
                        format!("cannot read {}: {e}", manifest_path.display()),
                    )
                    .note("a package root requires a `dowel.toml`"),
                );
                return None;
            }
        };

        let id = PackageId(self.packages.len());
        let manifest = self.parse_and_eval(manifest_file, true);
        let mut diags = Vec::new();
        let mut pkg =
            package::from_document(id, &manifest.doc, dir.to_path_buf(), manifest_file, &mut diags);
        self.diagnostics.append(&mut diags);

        let build_path = dir.join(BUILD_NAME);
        if build_path.exists() {
            match self.sm.load(&build_path) {
                Ok(f) => {
                    pkg.build_file = Some(f);
                    let build = self.parse_and_eval(f, false);
                    self.by_root.insert(dir.to_path_buf(), id);
                    self.packages.push(pkg);
                    self.build_targets(id, &build.doc);
                    log_debug!(
                        "loaded package `{}` from {}",
                        self.packages[id.0].name,
                        dir.display()
                    );
                    return Some(id);
                }
                Err(e) => self.diagnostics.push(Diagnostic::error(
                    "unreadable-build",
                    format!("cannot read {}: {e}", build_path.display()),
                )),
            }
        } else {
            self.diagnostics.push(
                Diagnostic::error("missing-build", format!("missing {}", build_path.display()))
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
        let out = query::evaluated(&self.db, file, strict)
            .expect("the session never cancels its own queries");
        self.diagnostics.extend(out.diagnostics.iter().cloned());
        out
    }

    /// `dowel.build` の各テーブルをターゲットへ組み上げる。
    fn build_targets(&mut self, pkg: PackageId, doc: &Document) {
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

            let block = match table.path.len() {
                2 => Block::Root,
                3 => match Block::parse(&table.path[2]) {
                    Some(b) => b,
                    None => {
                        let mut d = Diagnostic::error(
                            "unknown-block",
                            format!("unknown block `{}`", table.path[2]),
                        )
                        .at(doc.file, table.site.span, "only `public` or `private`")
                        .note("propagating and non-propagating properties are separated syntactically (docs/10-manifest.md)");
                        if let Some(c) = closest(&table.path[2], ["public", "private"]) {
                            d = d.suggest(
                                doc.file,
                                table.site.span,
                                format!("[{}.{}.{}]", kind.name(), name, c),
                                format!("did you mean `{c}`?"),
                            );
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

            let key = (kind.name().to_string(), name.clone());
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
                });
                tid
            });

            for entry in &table.entries {
                self.assign_prop(tid, block, entry.key.clone(), entry.value.clone(), entry.site);
            }
        }

        for t in &self.targets {
            if t.package == pkg {
                log_trace!("declared target {}.{}", t.kind.name(), t.name);
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
                if let Some(c) = closest(&name, names) {
                    d = d.suggest(
                        entry.site.file,
                        entry.site.span,
                        c,
                        format!("did you mean `{c}`?"),
                    );
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
            if let Some(c) = closest(head, known) {
                d = d.suggest(file, table.site.span, c, format!("did you mean `{c}`?"));
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
        value: Value,
        site: Site,
    ) {
        // `[lib.foo]` の中に `public.includes = ...` と書く形も許す。
        let (block, name) = match key.len() {
            1 => (block, key[0].clone()),
            2 if block == Block::Root => match Block::parse(&key[0]) {
                Some(b) => (b, key[1].clone()),
                None => (block, key.join(".")),
            },
            _ => (block, key.join(".")),
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
            if let Some(c) = closest(&name, known.iter().copied()) {
                d = d.suggest(site.file, site.span, c, format!("did you mean `{c}`?"));
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
    /// 取得を要する供給形態（レジストリ / git）は未実装のため `None` になる。
    pub fn dep_package(&self, from: PackageId, dep_name: &str) -> Option<PackageId> {
        let pkg = self.package(from);
        let dep = pkg.deps.iter().find(|d| d.name == dep_name)?;
        match &dep.kind {
            DepKind::Path(rel) => self.by_root.get(&canonical(&pkg.root.join(rel))).copied(),
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

/// 同じ名前が別ブロックに存在するか。診断の注記に使う。
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

/// 前回の実行が残した入力の記録。無ければ空。
fn read_inputs(root: &Path) -> Inputs {
    let path = Store::dir(root).join(INPUTS);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let inputs = Inputs::decode(&text);
            log_debug!("inputs: {} records from the previous run", inputs.len());
            inputs
        }
        Err(_) => Inputs::new(),
    }
}

/// 正規化に失敗しても落とさない。存在しないパスを指す診断は後段で出す。
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// 位置を持たない診断のための空スパン。
pub const NO_SPAN: Span = Span::EMPTY;
