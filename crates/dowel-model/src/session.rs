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
use crate::target::{
    label, ArtifactDecl, CaseDecl, HarnessDecl, PackageId, PropMap, Target, TargetDecl, TargetId,
};
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

/// 機能の転送が固定点に達するまでの走査の上限。
///
/// 集合は増える一方なので、実際にはパッケージ数を超えて回らない。
/// 上限を置くのは、想定外の形でも停止することを型の外で保証するため。
const MAX_FEATURE_ROUNDS: usize = 32;

pub const MANIFEST_NAME: &str = "dowel.toml";
pub const BUILD_NAME: &str = "dowel.build";

/// `[<kind>.<name>.artifacts]` の見出し。プロパティのブロックではない
/// （issue #60）。
///
/// 語そのものは `schema::NESTED_TABLES` が持つ。ここで綴り直すと、
/// `schema dump` とホバーが同じ表を知らないまま型検査だけが通る
/// （issue #90）。以下の3つも同じである。
const ARTIFACTS_BLOCK: &str = schema::ARTIFACTS;

/// `[<kind>.<name>.inspect]` の見出し。同じく、プロパティのブロックではない。
/// 変換との違いは出力を持たないことだけである（issue #60）。
const INSPECT_BLOCK: &str = schema::INSPECT;

/// `[test.<name>.cases]` の見出し。1本の実行ファイルから複数のテストを
/// 登録する。`test` 種別にのみ意味を持つ
const CASES_BLOCK: &str = schema::CASES;

/// `[test.<name>.harness]` の見出し。実行ファイル自身に事例を列挙させる宣言
const HARNESS_BLOCK: &str = schema::HARNESS;

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
    /// パッケージごとの有効な機能。`feature.<名前>` はここを引く（ADR-0017）
    active: BTreeMap<PackageId, std::collections::BTreeSet<String>>,
    /// パッケージのルート → そのパッケージに要求されている機能。
    /// 根には `--features`、依存には転送された名前が入る
    requested_features: BTreeMap<PathBuf, std::collections::BTreeSet<String>>,
    /// 外部への解決を済ませた (パッケージ, 依存名)。走査を繰り返しても
    /// 取得と pkg-config は1度しか行わない
    resolved_deps: std::collections::BTreeSet<(PackageId, String)>,
    /// 転送の記録。(転送先のルート, 機能名, 書かれた位置)。
    /// 全パッケージを読んでから、転送先がその機能を宣言しているか確かめる
    forwards: Vec<(PathBuf, String, Site)>,
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
            active: BTreeMap::new(),
            requested_features: BTreeMap::new(),
            resolved_deps: std::collections::BTreeSet::new(),
            forwards: Vec::new(),
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
            active: BTreeMap::new(),
            requested_features: BTreeMap::new(),
            resolved_deps: std::collections::BTreeSet::new(),
            forwards: Vec::new(),
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
            description: String::new(),
            id: pid,
            name: name.to_string(),
            version: r.version.clone(),
            root: PathBuf::new(),
            manifest_file: site.file,
            build_file: None,
            deps: Vec::new(),
            features: BTreeMap::new(),
            exclusive: Vec::new(),
            features_site: None,
            targets: Vec::new(),
            targets_site: None,
            toolchain: crate::package::ToolchainDecl::default(),
            toolchains: BTreeMap::new(),
            toolchains_path: None,
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
        // 外部のパッケージは宣言を持たない。pkg-config が答えた面だけを持つ
        // ターゲットとして置く。
        let mut decl = TargetDecl::bare(TableKind::Lib, name.to_string(), site);
        decl.public = public;
        let decl = std::sync::Arc::new(decl);
        query::set_target_source(
            &self.db,
            &label(&self.packages[pid.0].name, name),
            query::Source::External(decl.clone()),
        );
        self.targets.push(Target { id: tid, package: pid, decl });
        self.externals.insert(name.to_string(), pid);
        log_debug!("external dependency `{name}` {} via pkg-config", r.version);
    }

    /// 全パッケージの有効な機能を、`<パッケージ>/<機能>` の形で1つの集合に
    /// する。構成（`Config`）が持つ形であり、`feature.<名前>` の判定は
    /// 宣言されたパッケージで修飾して引く（ADR-0017）。
    pub fn active_features(&self) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for (pid, names) in &self.active {
            let pkg = &self.packages[pid.0].name;
            out.extend(names.iter().map(|f| format!("{pkg}/{f}")));
        }
        out
    }

    /// 読み込みの結果を構成へ載せる。
    ///
    /// 機能と版を1つの入口で載せる。別々にすると、片方だけを載せた構成が
    /// 作れてしまう——`pkg.version` が引けない構成では、`defines` に書いた
    /// 版が黙って消える（ADR-0020）。
    pub fn configure(&self, cfg: &mut dowel_eval::Config) {
        // 読み込みの段で解決した集合をそのまま使う。二重に求めると、
        // 「読み込んだ依存」と「有効な機能」が食い違いうる。
        cfg.features = self.active_features().clone();
        cfg.versions = self.packages.iter().map(|p| (p.name.clone(), p.version.clone())).collect();
    }

    /// 1つのパッケージで有効な機能。
    pub fn active_features_of(&self, id: PackageId) -> Option<&std::collections::BTreeSet<String>> {
        self.active.get(&id)
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
        self.requested_features.clear();
        self.resolved_deps.clear();
        self.forwards.clear();
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

    /// 読み込みに要る設定。導出クエリが互いを呼ぶときに持ち回る。
    fn ctx(&self) -> query::Ctx {
        query::Ctx { max_nesting: self.max_nesting, store: Some(self.cache.clone()) }
    }

    /// 依存側へ供給するプロパティ。メモを経由する。
    pub fn interface_of(&self, id: TargetId) -> Arc<query::Merged> {
        query::interface(&self.db, &self.label(id), &self.ctx())
            .expect("the session never cancels its own queries")
    }

    /// 自身のコンパイルに効くプロパティ。メモを経由する。
    pub fn compile_env_of(&self, id: TargetId) -> Arc<query::Merged> {
        query::compile_env(&self.db, &self.label(id), &self.ctx())
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

    /// パッケージを読み、依存を辿る。
    ///
    /// 機能はパッケージごとに解決する（ADR-0017）。依存元は `dep/feat` の形で
    /// 依存の機能を有効にできるため、あるパッケージに要求される機能は
    /// 「そのパッケージを使う全ての依存元」が決まるまで確定しない。しかも
    /// 転送された機能は依存の側の任意の依存を有効にしうるため、読み込みと
    /// 解決は互いに依存する。集合は増える一方なので、増えなくなるまで
    /// 走査を繰り返せば固定点に達する。
    ///
    /// 2周目以降に外部への副作用は無い。読み込みは `by_root`、git の取得と
    /// pkg-config の解決は `resolved_deps` が memo する。
    fn walk(&mut self) {
        self.requested_features
            .insert(self.root.clone(), self.features.requested.iter().cloned().collect());
        for _ in 0..MAX_FEATURE_ROUNDS {
            let before = self.requested_features.clone();
            self.walk_once();
            if self.requested_features == before {
                break;
            }
        }
        self.check_forwarded_features();
        self.check_exclusive_features();
    }

    /// 排他の宣言（`[features] exclusive`）が守られているか（issue #82）。
    ///
    /// 機能は加算である——`--features=x11` は `default` を落とさない。これは
    /// 規約として正しいが、実装の択一を条件付きの `sources` で書くと真正面から
    /// ぶつかる。両方立つと両方翻訳され、`bin` ならリンカの `multiple definition`、
    /// `lib` なら**組み上がって片方が黙って勝つ**。後者は成果物だけが違うものに
    /// なり、どちらが入ったかはリンカの解決順という記録の外のものが決める。
    ///
    /// 排他は推測しない。宣言されたものだけを見る。同じ記号を2つのファイルが
    /// 定義していることは、ここからは見えない。
    fn check_exclusive_features(&mut self) {
        let mut diags = Vec::new();
        for pkg in &self.packages {
            let Some(active) = self.active.get(&pkg.id) else { continue };
            for (group, site) in &pkg.exclusive {
                let on: Vec<&String> = group.iter().filter(|f| active.contains(*f)).collect();
                if on.len() < 2 {
                    continue;
                }
                let named = on.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(" and ");
                let mut d = Diagnostic::error(
                    "conflicting-features",
                    format!("{named} cannot be enabled at the same time"),
                )
                .at(site.file, site.span, "declared exclusive here");
                for line in self.why_active(pkg, &on) {
                    d = d.note(line);
                }
                diags.push(d.note("enable exactly one of them"));
            }
        }
        self.diagnostics.append(&mut diags);
    }

    /// なぜその機能が立っているのか。忘れやすいのは `default` の側である。
    fn why_active(&self, pkg: &Package, names: &[&String]) -> Vec<String> {
        let requested: Vec<String> =
            self.requested_features.get(&pkg.root).into_iter().flatten().cloned().collect();
        let by_default = package::resolve(pkg, &[], self.features.default).own;
        let by_request = package::resolve(pkg, &requested, false).own;
        let is_root = pkg.root == self.root;
        let mut out = Vec::new();
        for name in names {
            if by_default.contains(*name) {
                out.push(format!(
                    "`{name}` comes from `default`; `--no-default-features` drops it"
                ));
            } else if by_request.contains(*name) {
                out.push(if is_root {
                    format!("`{name}` was requested with `--features`")
                } else {
                    format!("`{name}` was enabled by a `{}/{name}` forward", pkg.name)
                });
            }
        }
        out
    }

    /// 転送先がその機能を宣言しているか確かめる。
    ///
    /// 宣言されていない名前は依存の側で偽と評価されるだけで、何も起きない。
    /// 転送を書いた人から見れば「有効にしたつもりのものが効かない」——
    /// 綴りを誤った転送と、意図して無効にした機能の区別が付かない
    /// （`check_feature_refs` と同じ理由、ADR-0017）。
    fn check_forwarded_features(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        let mut diags = Vec::new();
        for (dir, feat, site) in &self.forwards {
            let Some(&id) = self.by_root.get(dir) else { continue };
            if !seen.insert((id, feat.clone())) {
                continue;
            }
            let pkg = &self.packages[id.0];
            if pkg.features.contains_key(feat) {
                continue;
            }
            let declared: Vec<String> = pkg.features.keys().cloned().collect();
            diags.push(
                unknown_feature(feat, &declared, Some(*site), "this feature is forwarded here")
                    .note(format!("`{}` does not declare it in `[features]`", pkg.name)),
            );
        }
        self.diagnostics.append(&mut diags);
    }

    fn walk_once(&mut self) {
        let _phase = log::Phase::start("load");
        // 2つ目の要素は、その位置を読ませた宣言の位置。根には無い。
        // 読めなかった場合の診断がこれを指す。
        let mut queue: Vec<(PathBuf, Option<Site>)> = vec![(self.root.clone(), None)];
        let mut visited: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        while let Some((dir, from)) = queue.pop() {
            if !visited.insert(dir.clone()) {
                continue;
            }
            let id = match self.by_root.get(&dir) {
                Some(id) => *id,
                None => match self.load_package(&dir, from) {
                    Some(id) => id,
                    None => continue,
                },
            };
            // このパッケージの機能を解決し、依存への転送を控える。
            let requested: Vec<String> =
                self.requested_features.get(&dir).into_iter().flatten().cloned().collect();
            let resolved =
                package::resolve(&self.packages[id.0], &requested, self.features.default);
            log_debug!(
                "active features of `{}`: {}",
                self.packages[id.0].name,
                if resolved.own.is_empty() {
                    "(none)".to_string()
                } else {
                    resolved.own.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            );
            let own = resolved.own.clone();
            self.active.insert(id, resolved.own);
            self.forward_features(id, &dir, &resolved.forwarded);

            for dep in self.packages[id.0].deps.clone() {
                if !package::is_active(&dep, &own) {
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
                        // 取得と診断は1度だけ。走査は固定点まで繰り返される。
                        let first = self.resolved_deps.insert((id, dep.name.clone()));
                        if self.fetch && first {
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
                    // 書庫の依存（ADR-0029）。git と同じ形で、固定しているのが
                    // rev ではなく内容の指紋であるだけ。
                    DepKind::Tarball { url, sha256 } => {
                        let first = self.resolved_deps.insert((id, dep.name.clone()));
                        if self.fetch && first {
                            match crate::fetch::ensure_archive(
                                &self.root,
                                &dep.name,
                                url,
                                sha256,
                                dep.source_site,
                            ) {
                                Ok(d) => queue.push((canonical(&d), Some(dep.source_site))),
                                Err(d) => self.diagnostics.push(*d),
                            }
                        } else if let Some(d) =
                            crate::fetch::existing_archive(&self.root, &dep.name, sha256)
                        {
                            // エディタからは取得しない（git と同じ扱い）。
                            queue.push((canonical(&d), Some(dep.source_site)));
                        }
                    }
                    // `version` 依存はシステムの pkg-config で解決する（ADR-0015）。
                    // 解決結果は合成パッケージとして繋ぎ、dowel.lock と突き合わせる。
                    // エディタからは外部プロセスを起動しない（git と同じ扱い）。
                    DepKind::PkgConfig { min_version } => {
                        // pkg-config も1度だけ。外部プロセスを繰り返さない。
                        let first = self.resolved_deps.insert((id, dep.name.clone()));
                        if self.fetch && first {
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

    /// 転送された機能を、依存のパッケージの要求へ足す。
    ///
    /// 転送先は宣言された依存でなければならない。`dep("...")` が未宣言の
    /// パッケージを指すのと同じ誤りであり、同じコードで述べる。
    /// 転送先が `path` 以外（git / pkg-config）の場合、ここでは要求を
    /// 記録できるディレクトリが決まっていないことがある——その場合は
    /// 次の走査で解決済みのディレクトリに対して記録される。
    fn forward_features(
        &mut self,
        id: PackageId,
        dir: &Path,
        forwarded: &BTreeMap<String, Vec<(String, Site)>>,
    ) {
        for (dep_name, feats) in forwarded {
            let Some(dep) = self.packages[id.0].deps.iter().find(|d| &d.name == dep_name).cloned()
            else {
                // 1周目に限って述べる。繰り返しで重複させない。
                if self.resolved_deps.insert((id, format!("features/{dep_name}"))) {
                    let site = feats[0].1;
                    self.diagnostics.push(
                        Diagnostic::error(
                            "undeclared-dependency",
                            format!("`{dep_name}` is not a declared dependency"),
                        )
                        .at(site.file, site.span, "a feature forwards to this package")
                        .note(format!(
                            "`<dep>/<feature>` enables a feature of `<dep>`; declare `{dep_name}` in `[[dependencies]]`"
                        )),
                    );
                }
                continue;
            };
            let DepKind::Path(rel) = &dep.kind else {
                // 取得する依存は機能を持たない（合成ノードか、まだ手元に
                // 無い）。転送先が無いことは誤りとして述べない。
                continue;
            };
            let dep_dir = canonical(&dir.join(rel));
            for (feat, site) in feats {
                self.requested_features.entry(dep_dir.clone()).or_default().insert(feat.clone());
                self.forwards.push((dep_dir.clone(), feat.clone(), *site));
            }
        }
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
        self.read_shared_toolchains(&mut pkg, dir);

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
                    self.build_targets(id, f);
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

    /// `[package] toolchains` が指す共有の記述ファイルを読む
    /// （[ADR-0033](../../../docs/adr/0033-shared-toolchain-file.md)）。
    ///
    /// マニフェストと同じ道で読む。クエリ経由でなければ、記述ファイルを
    /// 直しても再評価されず、道具を替えたのに前の道具で組み続ける。
    ///
    /// `dowel.toml` の宣言を読んだ**後**に呼ぶ。補う向きの併合であり、
    /// 呼ぶ順がそのまま優先順位になる。
    fn read_shared_toolchains(&mut self, pkg: &mut package::Package, dir: &Path) {
        let Some((rel, site)) = pkg.toolchains_path.clone() else { return };
        let path = dir.join(&rel);
        let file = match self.read_source(&path) {
            Ok(f) => f,
            Err(e) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "unreadable-toolchains",
                        format!("cannot read {}: {e}", path.display()),
                    )
                    .at(site.file, site.span, "declared here")
                    .note("the path is relative to the `dowel.toml` that names it"),
                );
                return;
            }
        };
        let doc = self.parse_and_eval(file, true);
        let mut diags = Vec::new();
        package::read_toolchains(pkg, &doc.doc, file, &mut diags);
        // 記述ファイルは道具立てだけを持つ。他の表を黙って無視すると、
        // `dowel.toml` のつもりで書いたものが何も起きないまま通る。
        for t in &doc.doc.tables {
            if t.path.first().map(String::as_str) == Some("toolchain") {
                continue;
            }
            diags.push(
                Diagnostic::error(
                    "unknown-table",
                    format!("`[{}]` is not read from a toolchain file", t.path.join(".")),
                )
                .at(file, t.site.span, "this table has no meaning here")
                .note("a toolchain file holds `[toolchain]` and `[toolchain.<triple>]` only"),
            );
        }
        self.diagnostics.extend(diags);
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
    ///
    /// 組み上げ自体はメモを通す（[`query::build_decls`]）。同じ本文からは
    /// 同じ宣言が出るので、触っていないファイルはここで何も走らない——
    /// 以前は読み込みの度に値を写し、要約を取り直していた。
    fn build_targets(&mut self, pkg: PackageId, file: FileId) {
        let decls = query::build_decls(&self.db, file, &self.ctx())
            .expect("the session never cancels its own queries");
        for decl in &decls.targets {
            let id = TargetId(self.targets.len());
            self.targets.push(Target { id, package: pkg, decl: decl.clone() });
            query::set_target_source(
                &self.db,
                &label(&self.packages[pkg.0].name, &decl.name),
                query::Source::File(file),
            );
        }
        for (triple, runner) in &decls.runners {
            self.runners.insert(triple.clone(), runner.clone());
        }
        self.diagnostics.extend(decls.diagnostics.iter().cloned());
    }
}

/// `use` の値から、参照されたテンプレートの名前と位置を取り出す。
fn template_names(value: &Value) -> Vec<(String, Site)> {
    let mut out = Vec::new();
    collect_templates(value, &mut out);
    out
}

fn collect_templates(value: &Value, out: &mut Vec<(String, Site)>) {
    match &value.data {
        Data::List(items) => items.iter().for_each(|v| collect_templates(v, out)),
        Data::Template(name) => {
            if let Some(site) = value.prov.nearest_site() {
                out.push((name.clone(), site));
            }
        }
        _ => {}
    }
}

/// テンプレート由来の値を、既に在る値の**前**に置いて併合する（ADR-0035）。
fn prepend_props(into: &mut PropMap, from: &[PropMap], diags: &mut Vec<Diagnostic>) {
    let defs = schema::block_props();
    for def in defs {
        let mut reached: Vec<Value> = Vec::new();
        for m in from {
            if let Some(v) = m.get(def.name) {
                reached.push(v.clone());
            }
        }
        if reached.is_empty() {
            continue;
        }
        if let Some(own) = into.get(def.name) {
            reached.push(own.clone());
        }
        into.insert(def.name.to_string(), schema::merge_values(&def, &reached, diags));
    }
}

/// `dowel.build` のテーブル列をターゲットとランナーへ組み上げる先。
///
/// [`Session`] の読み込みと、開いている1ファイルだけを見る検査
/// （[`check_build_file`]、issue #38）が同じ実装を共有する。分けて持つと、
/// 片方だけを直したときに CLI とエディタの診断が黙って食い違う。
struct TargetSink<'a> {
    targets: &'a mut Vec<TargetDecl>,
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
    declarations_of(doc).diagnostics
}

/// 1つの `dowel.build` が宣言したもの一式。
///
/// 評価結果だけを見る純粋な関数である。だからこそ導出クエリに置ける
/// （[`crate::query::build_decls`]）——読み込みの度に組み上げていたものが、
/// ファイルが変わらない限り走らなくなる。
pub fn declarations_of(doc: &Document) -> crate::query::BuildDecls {
    let mut targets = Vec::new();
    let mut runners = BTreeMap::new();
    let mut diagnostics = Vec::new();
    TargetSink { targets: &mut targets, runners: &mut runners, diagnostics: &mut diagnostics }
        .build(doc);
    crate::query::BuildDecls {
        targets: targets.into_iter().map(std::sync::Arc::new).collect(),
        runners,
        diagnostics,
    }
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
        // `[lib.foo]` と `[lib.foo.public]` は別テーブルだが同じターゲットを指す。
        let mut index: BTreeMap<(String, String), TargetId> = BTreeMap::new();
        // 同じパッケージの中でターゲット名は一意である（issue #114）。
        // 名前が何に使われているかを見ると、一意でなければ成り立たない:
        // `target("foo")` の解決、`<パッケージ>:<ターゲット>` のラベル、
        // `obj/<パッケージ>/<ターゲット>/` の経路。同名を許すと、この3つ全てを
        // 種別で修飾する必要があり、得られるもの（`libfoo.a` と `foo` の同居）
        // に対して面が広すぎる。
        let mut declared: BTreeMap<String, (schema::TableKind, Site)> = BTreeMap::new();
        // 一度拒んだ組。`[bin.foo]` と `[bin.foo.private]` は別の表だが1つの
        // 誤りであり、表の数だけ診断を出すと本体が埋もれる。
        let mut refused: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();

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
            let is_cases = table.path.len() == 3 && table.path[2] == CASES_BLOCK;
            let is_harness = table.path.len() == 3 && table.path[2] == HARNESS_BLOCK;

            let block = match table.path.len() {
                2 => Block::Root,
                3 if is_artifacts || is_inspect || is_cases || is_harness => Block::Root,
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
                            "only `public`, `private`, `artifacts`, `inspect`, `cases`, or `harness`",
                        )
                        .note("propagating and non-propagating properties are separated syntactically (docs/10-manifest.md)");
                        if let (Some(c), Some(&span)) = (
                            closest(
                                &table.path[2],
                                [
                                    "public",
                                    "private",
                                    ARTIFACTS_BLOCK,
                                    INSPECT_BLOCK,
                                    CASES_BLOCK,
                                    HARNESS_BLOCK,
                                ],
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
                        )
                        .note(match table.path.get(2).map(|s| s.as_str()) {
                            // 一段深く書いてしまう先は、項目を持つブロックである。
                            // 「深すぎる」とだけ言われても、何が正しい形なのかは
                            // 読み取れない（issue #98）。
                            Some(CASES_BLOCK) | Some(ARTIFACTS_BLOCK) | Some(INSPECT_BLOCK) => {
                                format!(
                                    "the items of `{}` are inline tables inside it, not tables of their own: `<name> = {{ ... }}`",
                                    table.path[2]
                                )
                            }
                            _ => "propagating and non-propagating properties are separated syntactically (docs/10-manifest.md)".to_string(),
                        }),
                    );
                    continue;
                }
            };

            // 種別違いの同名は、2つ目を拒む。1つ目は正しく組まれる——
            // どちらも単独では正しい宣言なので、両方を落とす理由が無い。
            match declared.get(&name) {
                Some(&(other, first)) if other != kind => {
                    if !refused.insert((kind.name(), name.clone())) {
                        continue;
                    }
                    self.diagnostics.push(
                        Diagnostic::error(
                            "duplicate-target",
                            format!("`{name}` is already a {} target in this package", other.name()),
                        )
                        .at(
                            doc.file,
                            table.site.span,
                            format!("`{}` cannot reuse the name", kind.name()),
                        )
                        .with_label(dowel_support::Label::secondary(
                            first.file,
                            first.span,
                            "declared here first",
                        ))
                        .note("a target's name has to be unique in its package")
                        .note("`target(\"...\")`, the `<package>:<target>` label, and the object directory all key on it")
                        .note(format!(
                            "rename one, for example `[{}.{name}-{}]`",
                            kind.name(),
                            kind.name()
                        )),
                    );
                    continue;
                }
                _ => {
                    declared.entry(name.clone()).or_insert((kind, table.site));
                }
            }

            let tid = *index.entry(key).or_insert_with(|| {
                let tid = TargetId(self.targets.len());
                self.targets.push(TargetDecl::bare(kind, name.clone(), table.site));
                tid
            });

            if is_cases {
                self.declare_cases(tid, table);
                continue;
            }
            if is_harness {
                self.declare_harness(tid, table);
                continue;
            }
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

        self.expand_templates();

        for t in self.targets.iter() {
            log_trace!("declared target {}.{}", t.kind.name(), t.name);
        }
    }

    /// `use = [template("...")]` を展開する（ADR-0035）。
    ///
    /// テンプレートの `public` は使う側の `public` へ、`private` は `private`
    /// へ入る。ここが「ソースの無い lib に依存する」書き方との違いであり、
    /// 種別が在る理由そのものである——lib では `public` しか伝播せず、
    /// 共有することと公開することが分けられない。
    ///
    /// 展開はテンプレートの値を**先に**置いて併合する。テンプレートの行が
    /// 使う側に先に書かれていた場合と同じであり、`append` の順序も
    /// `replace` の後勝ちも普段どおりに効く。併合の代数に特例を作らないので、
    /// `dowel why` は展開後も来歴を辿れる。
    fn expand_templates(&mut self) {
        let templates: BTreeMap<String, (PropMap, PropMap, Site)> = self
            .targets
            .iter()
            .filter(|t| t.kind == TableKind::Template)
            .map(|t| (t.name.clone(), (t.public.clone(), t.private.clone(), t.site)))
            .collect();

        let ids: Vec<TargetId> = self
            .targets
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind != TableKind::Template)
            .map(|(i, _)| TargetId(i))
            .collect();

        for tid in ids {
            let uses = match self.targets[tid.0].root.get("use") {
                Some(v) => template_names(v),
                None => continue,
            };
            let mut public_from: Vec<PropMap> = Vec::new();
            let mut private_from: Vec<PropMap> = Vec::new();
            for (name, site) in uses {
                match templates.get(&name) {
                    Some((public, private, _)) => {
                        public_from.push(public.clone());
                        private_from.push(private.clone());
                    }
                    None => {
                        let known: Vec<&str> = templates.keys().map(|s| s.as_str()).collect();
                        let mut d = Diagnostic::error(
                            "unknown-template",
                            format!("no template named `{name}`"),
                        )
                        .at(
                            site.file,
                            site.span,
                            "this template is not declared",
                        );
                        if known.is_empty() {
                            d = d.note("declare it as `[template.<name>]` in this file");
                        } else {
                            d = d.note(format!("declared templates: {}", known.join(", ")));
                            if let Some(c) = closest(&name, known.iter().copied()) {
                                d = d.suggest(
                                    site.file,
                                    site.span,
                                    format!("template({c:?})"),
                                    format!("did you mean `{c}`?"),
                                );
                            }
                        }
                        self.diagnostics.push(d);
                    }
                }
            }
            if public_from.is_empty() && private_from.is_empty() {
                continue;
            }
            let mut diags = Vec::new();
            let target = &mut self.targets[tid.0];
            prepend_props(&mut target.public, &public_from, &mut diags);
            prepend_props(&mut target.private, &private_from, &mut diags);
            self.diagnostics.append(&mut diags);
        }
    }

    /// `[test.<name>.cases]` / `[bench.<name>.cases]` を取り込む。
    ///
    /// 1本の実行ファイルから複数のテスト（計測）を登録する。事例を分けるのは
    /// 引数であり、翻訳の単位は増えない。走らせる種別以外には意味が無い——
    /// 黙って読み飛ばすと、書いた宣言が記録の外に落ちる。
    fn declare_cases(&mut self, tid: TargetId, table: &dowel_eval::Table) {
        let kind = self.targets[tid.0].kind;
        if !matches!(kind, schema::TableKind::Test | schema::TableKind::Bench) {
            self.diagnostics.push(
                Diagnostic::error(
                    "unknown-block",
                    format!("`cases` has no meaning on a `{}` target", kind.name()),
                )
                .at(table.site.file, table.site.span, "only `test` and `bench` targets register cases")
                .note("a case is another invocation of the same binary, run by `dowel test` or `dowel bench`"),
            );
            return;
        }
        // どちらも「事例は何か」に答えるものである。両方書かれていたら、
        // どちらが効いたのかがマニフェストから読めない。
        if self.targets[tid.0].harness.is_some() {
            self.diagnostics.push(both_answer_what_the_cases_are(table.site));
            return;
        }
        // 事例を書く意図があって1つも残らなかったのは、書かなかったのとは
        // 別の状況である。0件の目標が黙って「引数なしで1回」になるより、
        // 書き手に決めさせる（issue #99）。
        if table.entries.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "empty-block",
                    format!("`[{}]` declares no case", table.path.join(".")),
                )
                .at(table.site.file, table.site.span, "this block is empty")
                .note("a target with no `cases` block is one test named after the target")
                .note("remove the block, or add at least one case"),
            );
            return;
        }
        let known = schema::case_props();
        let names: Vec<&str> = known.iter().map(|p| p.name).collect();
        for entry in &table.entries {
            let name = entry.key.join(".");
            if let Some(d) = invalid_case_name(&name, entry.site) {
                self.diagnostics.push(d);
                continue;
            }
            // 事例そのものが `match` / `when` を被っていてよい（issue #92）。
            // 検証は条件の葉——実際に登録されうるインライン表——それぞれに対して
            // 行う。条件は具体化まで解けないため、全ての枝が正しい必要がある。
            let leaves = case_tables(&entry.value);
            if leaves.is_empty() {
                self.diagnostics.push(
                    Diagnostic::error(
                        "type-mismatch",
                        format!(
                            "`{name}` is an inline table but {} was given",
                            entry.value.ty.display()
                        ),
                    )
                    .at(entry.site.file, entry.site.span, "expected `{ args = [...], ... }`")
                    .note("write for example `parse = { args = [\"parse\"], timeout = 10 }`")
                    .note("a case may be wrapped in `match` / `when`, but every arm has to be an inline table"),
                );
                continue;
            }
            for fields in leaves {
                for (prop, value) in fields {
                    // 誤っている値そのものに下線を引く。事例全体を指すと、
                    // どの鍵が悪いのか読み手が探すことになる（issue #101）。
                    let site = value.prov.nearest_site().unwrap_or(entry.site);
                    match known.iter().find(|p| p.name == prop) {
                        Some(def) if !def.ty.accepts(&value.ty) => self.diagnostics.push(
                            Diagnostic::error(
                                "type-mismatch",
                                format!(
                                    "`{prop}` is {} but {} was given",
                                    def.ty.display(),
                                    value.ty.display()
                                ),
                            )
                            .at(
                                site.file,
                                site.span,
                                format!("this value has type {}", value.ty.display()),
                            ),
                        ),
                        // 0 と負は「待ち続ける」に落ちる。時間切れを書いた意図と
                        // 正反対であり、黙って落ちる形が一番悪い（issue #96）。
                        Some(def) if def.name == "timeout" => {
                            if let Data::Int(n) = value.data {
                                if n <= 0 {
                                    self.diagnostics.push(
                                        Diagnostic::error(
                                            "invalid-value",
                                            format!("`timeout = {n}` is not a duration"),
                                        )
                                        .at(site.file, site.span, "a timeout is a positive number of seconds")
                                        .note("without a timeout dowel waits; writing 0 does not mean \"do not wait\""),
                                    );
                                }
                            }
                        }
                        // 計測に判定は無い。`should_fail` を黙って無視すると、
                        // 書いた宣言が「効いているように見えて効かない」。
                        Some(def)
                            if def.name == "should_fail" && kind == schema::TableKind::Bench =>
                        {
                            self.diagnostics.push(
                                Diagnostic::error(
                                    "unknown-property",
                                    "`should_fail` has no meaning in a bench case".to_string(),
                                )
                                .at(site.file, site.span, "a benchmark is measured, not judged")
                                .note("a bench case fails when a run exits nonzero; there is no verdict to invert"),
                            );
                        }
                        Some(_) => {}
                        None => {
                            let mut d = Diagnostic::error(
                                "unknown-property",
                                format!("unknown property `{prop}`"),
                            )
                            .at(site.file, site.span, "a case has no such property")
                            .note(format!("a case accepts: {}", names.join(", ")));
                            if let Some(c) = closest(prop, names.iter().copied()) {
                                d = d.note(format!("did you mean `{c}`?"));
                            }
                            self.diagnostics.push(d);
                        }
                    }
                }
            }
            self.targets[tid.0].cases.push(CaseDecl {
                name,
                value: entry.value.clone(),
                site: entry.site,
            });
        }
    }

    /// `[test.<name>.harness]` を取り込む（ADR-0023）。
    ///
    /// 事例の在り処が実行ファイルの中である場合の宣言である。dowel は枠組みを
    /// 1つも知らず、「どう尋ねるか」だけをここから読む。
    fn declare_harness(&mut self, tid: TargetId, table: &dowel_eval::Table) {
        if self.targets[tid.0].kind != schema::TableKind::Test {
            let kind = self.targets[tid.0].kind.name();
            self.diagnostics.push(
                Diagnostic::error(
                    "unknown-block",
                    format!("`harness` has no meaning on a `{kind}` target"),
                )
                .at(table.site.file, table.site.span, "only `test` targets have a harness")
                .note("a harness is how `dowel test` asks a binary what cases it contains"),
            );
            return;
        }
        if !self.targets[tid.0].cases.is_empty() {
            self.diagnostics.push(both_answer_what_the_cases_are(table.site));
            return;
        }
        let known = schema::harness_props();
        let names: Vec<&str> = known.iter().map(|p| p.name).collect();
        let mut fields = std::collections::BTreeMap::new();
        for entry in &table.entries {
            let prop = entry.key.join(".");
            match known.iter().find(|p| p.name == prop) {
                Some(def) if !def.ty.accepts(&entry.value.ty) => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "type-mismatch",
                            format!(
                                "`{prop}` is {} but {} was given",
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
                Some(_) => {}
                None => {
                    let mut d =
                        Diagnostic::error("unknown-property", format!("unknown property `{prop}`"))
                            .at(entry.site.file, entry.site.span, "a harness has no such property")
                            .note(format!("a harness accepts: {}", names.join(", ")));
                    if let Some(c) = closest(&prop, names.iter().copied()) {
                        d = d.note(format!("did you mean `{c}`?"));
                    }
                    self.diagnostics.push(d);
                    continue;
                }
            }
            fields.insert(prop, entry.value.clone());
        }
        // 列挙のしかたが無ければ、この宣言は何も述べていない。既定を当てると
        // 「どう尋ねたのか」がマニフェストから読めなくなる。
        if !fields.contains_key("list") {
            self.diagnostics.push(
                Diagnostic::error("missing-field", "`[harness]` has no `list`")
                    .at(table.site.file, table.site.span, "write `list = [\"--list\"]`")
                    .note(
                        "dowel knows no test framework; the arguments that print the case names \
                         have to be declared",
                    ),
            );
            return;
        }
        self.targets[tid.0].harness = Some(HarnessDecl { fields, site: table.site });
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
                                .map(|(n, _, _)| *n)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                );
                continue;
            };
            let Some(tool_name) = tool.as_str() else { continue };

            // 道具の名前は表が決める。実体の選択は `[toolchain]` の仕事。
            let tools: Vec<&str> = dowel_eval::config::TOOLS.iter().map(|(n, _, _)| *n).collect();
            if !tools.contains(&tool_name) {
                let mut d = Diagnostic::error(
                    "unknown-tool",
                    format!("`{tool_name}` is not a toolchain tool"),
                )
                .at(entry.site.file, entry.site.span, "no such tool")
                .note(format!("declarable tools: {}", tools.join(", ")))
                .note("write the tool's name here, not the command: `[toolchain]` supplies the command");
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
                .note(format!(
                    "implemented kinds: {}",
                    TableKind::ALL
                        .iter()
                        .filter(|k| k.is_implemented())
                        .map(|k| k.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
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

        // テンプレートは設定だけを持つ（ADR-0035）。root のプロパティは
        // 「そのターゲットが何であるか」を決めるもので、共有すると何を
        // 作っているのかが読み取れなくなる。
        if self.targets[tid.0].kind == TableKind::Template && block == Block::Root {
            self.diagnostics.push(
                Diagnostic::error("unknown-property", format!("a template has no `{name}`"))
                    .at(site.file, site.span, "templates hold settings only")
                    .note("write it in the target that uses this template")
                    .note(format!(
                        "`[template.<name>.public]` and `.private` accept: {}",
                        schema::prop_names(Block::Public).join(", ")
                    )),
            );
            return;
        }

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

        // ABI 札を成分で書いた場合、成分の名前と値も閉じた語彙である
        // （ADR-0042）。綴りを誤った成分は、どちらの側も名指していない
        // ことになり、比べられずに素通りする——制約を書いたつもりの記述が
        // 何も制約しない。
        if def.ty == dowel_eval::Type::AbiLabel {
            if let dowel_eval::Data::Map(m) = &value.data {
                for (component, item) in m {
                    if let Some(d) = self.abi_component_diagnostic(component, item, site) {
                        self.diagnostics.push(d);
                        return;
                    }
                }
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

    /// ABI 札の1成分を語彙に照らす（ADR-0042）。
    ///
    /// 名前も値も閉じている。開いていると、綴りを誤った成分は「片方しか
    /// 名指していない成分」として扱われ、比べられずに通る——制約を書いた
    /// つもりの記述が、何も制約しない。
    fn abi_component_diagnostic(
        &self,
        component: &str,
        item: &Value,
        site: Site,
    ) -> Option<Diagnostic> {
        use dowel_eval::schema::{abi_component, ABI_COMPONENTS};
        let at = item.prov.nearest_site().unwrap_or(site);
        let Some((_, _, domain)) = abi_component(component) else {
            let known: Vec<&str> = ABI_COMPONENTS.iter().map(|(n, _, _)| *n).collect();
            let mut d = Diagnostic::error(
                "unknown-abi-component",
                format!("`{component}` is not an `abi` component"),
            )
            // 位置は札の全体を指す。表の鍵は綴りを保っておらず、値だけを
            // 指すと「知らない成分」と言いながら値に下線が引かれる。
            .at(site.file, site.span, format!("`{component}` is written here"))
            .note(format!("`abi` accepts: {}", known.join(", ")))
            .note("the vocabulary is closed and grows one component at a time (ADR-0042)");
            if let Some(c) = closest(component, known) {
                d = d.note(format!("did you mean `{c}`?"));
            }
            return Some(d);
        };
        let text = item.as_str()?;
        if domain.contains(&text) {
            return None;
        }
        let mut d = Diagnostic::error(
            "unknown-abi-component",
            format!("`{text}` is not a value of the `abi` component `{component}`"),
        )
        .at(at.file, at.span, "not a known value")
        .note(format!("`{component}` accepts: {}", domain.join(", ")));
        if let Some(c) = closest(text, domain.iter().copied()) {
            d = d.suggest(at.file, at.span, format!("\"{c}\""), format!("did you mean `{c}`?"));
        }
        Some(d)
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
            DepKind::Tarball { sha256, .. } => self
                .by_root
                .get(&canonical(&crate::fetch::archive_dir(&self.root, &dep.name, sha256)))
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

    /// 成果物を作る全ターゲット（issue #141）。
    ///
    /// 「全部を検査する」入口——`check`、`migrate verify`、言語サーバ——が
    /// 読む一覧である。ここに雛型や実行ラッパを混ぜると、計画は名指しされた
    /// と受け取り、`not-a-target` を出す。3箇所が各々 `sess.targets` を
    /// そのまま数えていたので、3箇所とも同じ誤りを持っていた。
    pub fn buildable_targets(&self) -> Vec<TargetId> {
        self.targets.iter().filter(|t| t.kind.is_target()).map(|t| t.id).collect()
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

/// `cases` と `harness` が両方書かれた。
///
/// どちらも「このターゲットの事例は何か」に答えるものであり、両立させると
/// どちらが効いたのかがマニフェストから読めない（ADR-0023）。
fn both_answer_what_the_cases_are(site: Site) -> Diagnostic {
    Diagnostic::error("conflicting-declaration", "`cases` and `harness` cannot both be declared")
        .at(site.file, site.span, "the other one is declared too")
        .note("both answer what the cases of this target are")
        .note("`cases` registers them in the manifest; `harness` asks the binary")
}

/// 事例の値から、登録されうるインライン表を全て取り出す。
///
/// 事例そのものは `match` / `when` を被っていてよい（issue #92）。条件は
/// 具体化まで解けないので、検証は全ての枝に対して行う。1つも表が無ければ、
/// この値は事例になりえない。
fn case_tables(v: &Value) -> Vec<&std::collections::BTreeMap<String, Value>> {
    match &v.data {
        Data::Map(fields) => vec![fields],
        Data::When { inner, .. } => case_tables(inner),
        Data::Match { arms, .. } => arms.iter().flat_map(|a| case_tables(&a.value)).collect(),
        _ => Vec::new(),
    }
}

/// 事例の名前がラベルの文法を壊さないか（issue #97）。
///
/// ラベルは `<パッケージ>:<ターゲット>/<事例>` であり、要約・JSON・`--failed`・
/// 位置引数がこれを識別子として読む。`/` は目標と事例の区切りであり、空白は
/// 消費者の区切りであり、空名は目標の綴りと1文字しか違わない。
fn invalid_case_name(name: &str, site: Site) -> Option<Diagnostic> {
    let what = case_name_problem(name)?;
    Some(
        Diagnostic::error("invalid-name", format!("`{name}` cannot be a case name"))
            .at(site.file, site.span, what)
            .note("the case's label is `<package>:<target>/<case>`")
            .note("the summary, the JSON output, `--failed`, and the command line all read it")
            .note("use `-` or `_` where a separator is wanted"),
    )
}

/// 事例の名前がラベルの文法を壊すか。壊すなら、その理由。
///
/// 名前は2つの入口から来る——マニフェストと、ハーネスの列挙（ADR-0023）。
/// **受け入れる文法は入口によらず1つ**であり、規則をどちらか一方に持つと、
/// もう一方から同じ壊れ方が入る（issue #108）。診断に包む側とそうでない側が
/// あるので、判定だけをここに置く。
pub fn case_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("a case needs a name")
    } else if name.contains('/') {
        Some("`/` separates the target from the case in `<package>:<target>/<case>`")
    } else if name.chars().any(char::is_whitespace) {
        Some("whitespace splits the label for anything that reads it by words")
    } else {
        None
    }
}
