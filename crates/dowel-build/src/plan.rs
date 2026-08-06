//! アクショングラフの構築。
//!
//! ここが「構成を与えて具体化する」唯一の場所である。マニフェスト評価は
//! 構成を知らず、`Cfg<T>` のまま値を持っている（docs/10-manifest.md 3節）。

use crate::action::{Action, ActionId, ActionKind};
use crate::glob;
use dowel_eval::schema::TableKind;
use dowel_eval::{Config, Data, Opt, PathBase, Value};
use dowel_model::graph::Graph;
use dowel_model::interface;
use dowel_model::{Session, TargetId};
use dowel_support::{log_debug, log_trace, Diagnostic};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// `compile_commands.json` の1件。
#[derive(Clone, Debug)]
pub struct CompileCommand {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub arguments: Vec<String>,
    pub output: PathBuf,
}

pub struct Plan {
    pub build_dir: PathBuf,
    pub actions: Vec<Action>,
    /// ターゲット → 最終成果物
    pub artifacts: BTreeMap<TargetId, PathBuf>,
    /// ターゲット → 成果物から派生させたもの（`artifacts` ブロック、issue #60）。
    /// 宣言順。これらもビルドの成果物であり、既定で作られる
    pub derived: BTreeMap<TargetId, Vec<PathBuf>>,
    pub compile_commands: Vec<CompileCommand>,
    /// 要求されたターゲット
    pub requested: Vec<TargetId>,
}

impl Plan {
    /// ビルドが作るもの。ninja の `default` と「何を作ったか」の表示が読む。
    ///
    /// 要求されたターゲットの成果物と、**計画に載った全ターゲット**の派生で
    /// ある。派生をこの計画の全体から採るのは、それがそのターゲット自身の
    /// 出力だからである。依存として書庫が作られるなら、その隣に置くと
    /// 宣言された `.stripped` も作られる——派生が出るかどうかが、自分の
    /// 宣言ではなく「誰かが自分に依存しているか」で決まってはならない
    /// （issue #64）。
    ///
    /// 派生は誰の入力にもならないため、ninja からは `default` に並べない限り
    /// 到達しない。一方 direct 実行器は全アクションを走らせる。並べなければ
    /// 実行器によって出来上がるものが違う（issue #41 と同じ形）。
    pub fn default_outputs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for t in &self.requested {
            out.extend(self.artifacts.get(t).cloned());
        }
        for derived in self.derived.values() {
            out.extend(derived.iter().cloned());
        }
        out
    }

    pub fn action(&self, id: ActionId) -> &Action {
        &self.actions[id.0]
    }

    /// 依存が先に来る順。直接実行はこの順で走らせる。
    pub fn order(&self) -> Vec<ActionId> {
        // 構築時に依存が先に積まれているため、そのままの順で足りる。
        // 不変条件として検査しておく。
        for a in &self.actions {
            debug_assert!(
                a.deps.iter().all(|d| d.0 < a.id.0),
                "an action depends on a later action"
            );
        }
        self.actions.iter().map(|a| a.id).collect()
    }
}

/// ビルドディレクトリ。構成ごとに分ける。
pub fn build_dir(root: &Path, cfg: &Config) -> PathBuf {
    root.join(".dowel").join("build").join(cfg.id())
}

pub fn plan(
    sess: &Session,
    graph: &Graph,
    cfg: &Config,
    requested: &[TargetId],
) -> (Plan, Vec<Diagnostic>) {
    let _phase = dowel_support::log::Phase::start("plan");
    let mut diags = Vec::new();
    let root = sess.root_package().map(|p| p.root.clone()).unwrap_or_else(|| PathBuf::from("."));
    let build_dir = build_dir(&root, cfg);

    // ツールチェーンの宣言はターゲットトリプルで引く。無印の `[toolchain]` は
    // ホスト向け、`[toolchain.<triple>]` はそのトリプル向けである（issue #42）。
    let host = dowel_eval::config::default_triple();
    let root_toolchain = sess.root_package().and_then(|p| p.toolchain_for(&cfg.target, &host));

    // ツールチェーンが混ざると ABI の前提が崩れる。1回のビルドで1つに限る。
    // 比較するのは今のターゲットトリプルに適用される宣言だけ。別トリプル向けの
    // 宣言は、このビルドに対する要求ではない。
    for p in &sess.packages {
        let Some(decl) = p.toolchain_for(&cfg.target, &host) else { continue };
        for (name, _) in dowel_eval::config::TOOLS {
            let Some(t) = decl.tool(name) else { continue };
            let used = cfg.tool(name);
            if t.command != used {
                diags.push(
                    Diagnostic::warning(
                        "toolchain-mismatch",
                        format!(
                            "package `{}` asks for `{name} = \"{}\"` but the build uses `{used}`",
                            p.name, t.command
                        ),
                    )
                    .note("fetching and switching toolchains is Phase 5 (docs/90-roadmap.md)")
                    .note("ABI label checking assumes a single pinned toolchain"),
                );
            }
        }
    }

    // 固定した対象が実在するかどうかは、記録されない入力を排除する前提である
    // （docs/00-overview.md 2節）。確かめなければ `/bin/sh: not found` が
    // ビルドの失敗として出るだけで、`[toolchain]` のどの行が原因かを示さない。
    // C は常に要る。他の道具はそれを使う箇所が要求する（require_tool）。
    require_tool(&mut diags, cfg, root_toolchain, "c", "C compiler");

    // 必要なターゲットの集合。要求されたものとその推移的依存。
    let mut needed: BTreeSet<TargetId> = BTreeSet::new();
    for &t in requested {
        needed.extend(graph.link_closure(t));
    }

    let mut plan = Plan {
        build_dir: build_dir.clone(),
        actions: Vec::new(),
        artifacts: BTreeMap::new(),
        derived: BTreeMap::new(),
        compile_commands: Vec::new(),
        requested: requested.to_vec(),
    };
    // ターゲット → そのターゲットの成果物を作るアクション
    let mut producer: BTreeMap<TargetId, ActionId> = BTreeMap::new();
    // ターゲット → 自身のソースに C++ を含むか。リンカの選択が読む
    let mut has_cxx: BTreeMap<TargetId, bool> = BTreeMap::new();
    // C++ コンパイラの実在検査は C++ ソースが現れたときに1度だけ行う。
    // C だけのビルドに C++ ツールチェーンを要求しないため
    let mut cxx_toolchain_checked = false;
    // 書庫作成器も同じ扱い。書庫を作らないビルドには要求しない
    let mut ar_toolchain_checked = false;
    // 変換の道具も同じ扱い。使う宣言があったときに1度だけ確かめる
    let mut probed_tools: BTreeSet<String> = BTreeSet::new();

    // `graph.order` は依存が先。成果物ができてからリンクする順になる。
    for &tid in &graph.order {
        if !needed.contains(&tid) {
            continue;
        }
        let target = sess.target(tid);
        let pkg = sess.package(target.package);
        let env = interface::compile_env(sess, tid, &mut diags);

        let sources = collect_sources(sess, tid, cfg, &mut diags);
        has_cxx.insert(tid, sources.iter().any(|s| is_cxx(s)));
        if has_cxx[&tid] && !cxx_toolchain_checked {
            cxx_toolchain_checked = true;
            if cfg.target != host && root_toolchain.is_none_or(|t| t.tool("cxx").is_none()) {
                // ホストの `c++` へ落とすと、C++ の翻訳単位だけ別アーキテクチャの
                // オブジェクトになる。黙って組まず、宣言の不足として述べる。
                let mut d = Diagnostic::error(
                    "missing-toolchain",
                    format!("no C++ toolchain is declared for target `{}`", cfg.target),
                );
                if let Some(s) = root_toolchain.and_then(|t| t.site) {
                    d = d.at(s.file, s.span, "add `cxx = \"...\"` here");
                }
                diags.push(d.note(
                    "the sources contain C++, and the host `c++` would produce objects for the wrong architecture",
                ));
            } else {
                require_tool(&mut diags, cfg, root_toolchain, "cxx", "C++ compiler");
            }
        }
        if sources.is_empty() && target.kind != TableKind::Lib {
            diags.push(
                Diagnostic::error("no-sources", format!("`{}` has no sources", sess.label(tid)))
                    .at(target.site.file, target.site.span, "`sources` is empty")
                    .note("set it, for example `sources = glob(\"src/*.c\")`"),
            );
            continue;
        }

        let includes = collect_includes(sess, &env, &build_dir, &mut diags);
        let defines = collect_defines(&env);
        let flags = collect_flags(&env, "flags");
        // 言語標準は型付きのプロパティであり、`-std=` はここで組み立てる。
        // 言語別のフラグより前に置く。`c_flags = ["-std=gnu11"]` のような
        // 方言の指定が後に来て勝つようにするため（後勝ちは -std の慣習）
        let mut c_flags = std_flag(&env, "c_std").into_iter().collect::<Vec<_>>();
        c_flags.extend(collect_flags(&env, "c_flags"));
        let mut cxx_flags = std_flag(&env, "cxx_std").into_iter().collect::<Vec<_>>();
        cxx_flags.extend(collect_flags(&env, "cxx_flags"));
        // `link_flags` だけは compile_env からではなく、リンク閉包から集める。
        // `private` はリンクの到達可能性を制御しない（issue #56、下の
        // `closure_link_flags`）。
        let link_flags = closure_link_flags(sess, graph, cfg, tid, &build_dir, &mut diags);

        log_debug!(
            "{}: {} sources, {} includes, {} defines",
            sess.label(tid),
            sources.len(),
            includes.len(),
            defines.len()
        );
        // 中間結果を丸ごと出す。コンパイル引数が期待と違うとき、
        // どの段階で入り込んだかはここを見れば分かる。
        for s in &sources {
            log_trace!("  source  {}", s.display());
        }
        for i in &includes {
            log_trace!("  include {}", i.display());
        }
        for (k, v) in &defines {
            log_trace!("  define  {k}={v}");
        }
        if !flags.is_empty() {
            log_trace!("  flags   {}", flags.join(" "));
        }
        if !c_flags.is_empty() {
            log_trace!("  cflags  {}", c_flags.join(" "));
        }
        if !cxx_flags.is_empty() {
            log_trace!("  cxxflags {}", cxx_flags.join(" "));
        }
        if !link_flags.is_empty() {
            log_trace!("  ldflags {}", link_flags.join(" "));
        }

        // --- コンパイル ---
        let mut objects = Vec::new();
        let mut compile_ids = Vec::new();
        for src in &sources {
            // 言語は拡張子で決まる。`cc` は driver であり `.cpp` のコンパイル
            // 自体は通すが、C++ として組むには標準ライブラリと ABI の前提が
            // 揃った driver（`tc.cxx`）を使う必要がある
            let (compiler, tool) =
                if is_cxx(src) { (cfg.tool("cxx"), "CXX") } else { (cfg.tool("c"), "CC") };
            let obj = object_path(&build_dir, &pkg.name, &target.name, &pkg.root, src);
            let depfile = obj.with_extension("o.d");
            let mut args: Vec<String> = Vec::new();
            args.extend(default_compile_flags(cfg));
            args.extend(flags.iter().cloned());
            // 言語別のフラグは共通の `flags` の後。後勝ちの慣習により、
            // 言語別の指定が共通の指定を上書きできる向きにする
            args.extend(if is_cxx(src) { &cxx_flags } else { &c_flags }.iter().cloned());
            for inc in &includes {
                args.push(format!("-I{}", inc.display()));
            }
            for (k, v) in &defines {
                args.push(if v.is_empty() { format!("-D{k}") } else { format!("-D{k}={v}") });
            }
            args.push("-MD".into());
            args.push("-MF".into());
            args.push(depfile.display().to_string());
            args.push("-c".into());
            args.push(src.display().to_string());
            args.push("-o".into());
            args.push(obj.display().to_string());

            let id = ActionId(plan.actions.len());
            plan.actions.push(Action {
                id,
                kind: ActionKind::Compile,
                target: tid,
                program: compiler.to_string(),
                args: args.clone(),
                inputs: vec![src.clone()],
                outputs: vec![obj.clone()],
                depfile: Some(depfile),
                description: format!("{tool} {}", rel_display(&build_dir, &obj)),
                deps: Vec::new(),
            });
            let mut arguments = vec![compiler.to_string()];
            arguments.extend(args);
            plan.compile_commands.push(CompileCommand {
                directory: build_dir.clone(),
                file: src.clone(),
                arguments,
                output: obj.clone(),
            });
            log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
            objects.push(obj);
            compile_ids.push(id);
        }

        // --- 集約（アーカイブ／リンク） ---
        match target.kind {
            TableKind::Lib => {
                // 書庫作成器の実在検査は、書庫を作るときに1度だけ行う。
                // コンパイラと同じく、固定した対象の実在は計画段で確かめる
                // （issue #50）。
                if !ar_toolchain_checked {
                    ar_toolchain_checked = true;
                    require_tool(&mut diags, cfg, root_toolchain, "ar", "archiver");
                }
                let out = build_dir.join("lib").join(format!("lib{}.a", target.name));
                let mut args = vec!["rcs".to_string(), out.display().to_string()];
                args.extend(objects.iter().map(|o| o.display().to_string()));
                let id = ActionId(plan.actions.len());
                plan.actions.push(Action {
                    id,
                    kind: ActionKind::Archive,
                    target: tid,
                    program: cfg.tool("ar").to_string(),
                    args,
                    inputs: objects.clone(),
                    outputs: vec![out.clone()],
                    depfile: None,
                    description: format!("AR {}", rel_display(&build_dir, &out)),
                    deps: compile_ids.clone(),
                });
                log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
                producer.insert(tid, id);
                plan.artifacts.insert(tid, out);
            }
            TableKind::Bin | TableKind::Test => {
                let out = build_dir.join("bin").join(&target.name);
                // リンク閉包のどこかに C++ の翻訳単位があれば、リンクは C++ の
                // driver で行う。C の driver では C++ 標準ライブラリが付かず、
                // 未定義参照は原因（依存先のソース）から離れた場所で報告される
                let link_needs_cxx = graph
                    .link_closure(tid)
                    .into_iter()
                    .any(|t| has_cxx.get(&t).copied().unwrap_or(false));
                let linker = if link_needs_cxx { cfg.tool("cxx") } else { cfg.tool("c") };
                // リンク順は依存元が先。静的ライブラリの解決順の要請による。
                let libs: Vec<PathBuf> = graph
                    .link_closure(tid)
                    .into_iter()
                    .filter(|t| *t != tid)
                    .filter_map(|t| plan.artifacts.get(&t).cloned())
                    .collect();
                let mut args: Vec<String> =
                    objects.iter().map(|o| o.display().to_string()).collect();
                args.extend(libs.iter().map(|l| l.display().to_string()));
                args.extend(link_flags.iter().cloned());
                args.push("-o".into());
                args.push(out.display().to_string());

                let mut inputs = objects.clone();
                inputs.extend(libs.iter().cloned());
                let mut deps = compile_ids.clone();
                deps.extend(
                    graph
                        .link_closure(tid)
                        .into_iter()
                        .filter(|t| *t != tid)
                        .filter_map(|t| producer.get(&t).copied()),
                );

                let id = ActionId(plan.actions.len());
                plan.actions.push(Action {
                    id,
                    kind: ActionKind::Link,
                    target: tid,
                    program: linker.to_string(),
                    args,
                    inputs,
                    outputs: vec![out.clone()],
                    depfile: None,
                    description: format!("LINK {}", rel_display(&build_dir, &out)),
                    deps,
                });
                log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
                producer.insert(tid, id);
                plan.artifacts.insert(tid, out);
            }
            other => diags.push(
                Diagnostic::error(
                    "unimplemented-kind",
                    format!("cannot produce an artifact for `{}`", other.name()),
                )
                .at(target.site.file, target.site.span, "unimplemented kind"),
            ),
        }

        // --- 成果物からの派生（`artifacts` ブロック、issue #60） ---
        for decl in &target.artifacts {
            let (Some(input), Some(&producer_id)) =
                (plan.artifacts.get(&tid).cloned(), producer.get(&tid))
            else {
                // 成果物を作れなかった種類。既に診断は出ている。
                break;
            };
            if probed_tools.insert(decl.tool.clone()) {
                require_tool(
                    &mut diags,
                    cfg,
                    root_toolchain,
                    &decl.tool,
                    &format!("{} tool", decl.tool),
                );
            }
            // 出力は成果物の拡張子を置き換えたもの。`firmware` → `firmware.bin`、
            // `libfoo.a` → `libfoo.bin`。書式文字列を持ち込まないための規則。
            let out = input.with_extension(&decl.suffix);
            let mut args: Vec<String> = decl
                .args
                .as_ref()
                .and_then(|v| dowel_eval::specialize(v, &cfg.for_package(&pkg.name)))
                .map(|v| {
                    flatten(&v).iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
                })
                .unwrap_or_default();
            // 入力と出力は末尾に位置で置く（ADR-0008）。
            args.push(input.display().to_string());
            args.push(out.display().to_string());

            let id = ActionId(plan.actions.len());
            plan.actions.push(Action {
                id,
                kind: ActionKind::Transform,
                target: tid,
                program: cfg.tool(&decl.tool).to_string(),
                args,
                inputs: vec![input],
                outputs: vec![out.clone()],
                depfile: None,
                description: format!(
                    "{} {}",
                    decl.tool.to_uppercase(),
                    rel_display(&build_dir, &out)
                ),
                deps: vec![producer_id],
            });
            log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
            plan.derived.entry(tid).or_default().push(out);
        }
    }

    log_debug!(
        "{} actions ({} compile, {} archive, {} link, {} transform)",
        plan.actions.len(),
        count(&plan, ActionKind::Compile),
        count(&plan, ActionKind::Archive),
        count(&plan, ActionKind::Link),
        count(&plan, ActionKind::Transform)
    );

    (plan, diags)
}

fn count(plan: &Plan, kind: ActionKind) -> usize {
    plan.actions.iter().filter(|a| a.kind == kind).count()
}

/// 構成から来る既定のフラグ。マニフェストの `flags` より前に置き、
/// 記述側が後から上書きできるようにする。
fn default_compile_flags(cfg: &Config) -> Vec<String> {
    match cfg.opt {
        Opt::Debug => vec!["-g".into(), "-O0".into()],
        Opt::Release => vec!["-O2".into(), "-DNDEBUG".into()],
    }
}

/// C++ として扱われる拡張子。
///
/// `cc` も `c++` も driver であり拡張子で言語を判別するが、選択の結果は
/// コンパイルだけでなくリンク（標準ライブラリの同伴）にも効くため、
/// こちらでも判別して driver とリンカを選ぶ。
const CXX_EXTENSIONS: &[&str] = &["cc", "cp", "cpp", "cxx", "c++", "CPP", "C"];

fn is_cxx(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| CXX_EXTENSIONS.contains(&e))
}

/// 道具の実在を確かめ、無ければ `missing-toolchain` を積む。
///
/// 診断の組み立ては道具に依らない（宣言があればその行を、無ければ既定に
/// 落ちた旨を指す）。**いつ呼ぶか**だけが道具ごとの判断である：C は常に、
/// C++ は C++ ソースが現れたとき、archiver は書庫を作るとき。要不要は
/// 道具を使う側の意味論であり、表には置かない。
fn require_tool(
    diags: &mut Vec<Diagnostic>,
    cfg: &Config,
    root_toolchain: Option<&dowel_model::package::ToolchainDecl>,
    name: &str,
    what: &str,
) {
    let command = cfg.tool(name);
    if crate::exec::program_exists(command) {
        return;
    }
    let mut d =
        Diagnostic::error("missing-toolchain", format!("cannot find the {what} `{command}`"));
    match root_toolchain.and_then(|t| t.tool(name)) {
        Some(t) => d = d.at(t.site.file, t.site.span, "declared here"),
        None => {
            d = d.note(format!(
                "no `[toolchain] {name}` is declared, so the default `{}` is used",
                dowel_eval::config::default_tool(name)
            ))
        }
    }
    diags.push(d.note(
        "fetching toolchains is Phase 5 (docs/90-roadmap.md); until then it must be on PATH",
    ));
}

/// リンク閉包から集めた `link_flags`。
///
/// 静的な書庫は自分のリンク要件を運べない。書庫が閉包を辿って最終リンクに
/// 乗る（`libs` の収集）以上、その書庫が要求する `link_flags` も同じ閉包を
/// 辿らなければ、書庫だけがあって記号が解けない（issue #56）。
/// `public` / `private` は**翻訳の伝播**（`includes` / `defines` / `flags`）を
/// 堰き止めるものであり、リンクの到達可能性は制御しない。
///
/// 順序は依存元が先・依存が後（書庫と同じ、静的リンクの解決順の要請）。
/// 重複は畳まない——`link_flags` の併合規則は `append`（順序保持）であり、
/// 閉包の各ノードは一度しか現れないため、二重取りも起きない。
fn closure_link_flags(
    sess: &Session,
    graph: &Graph,
    cfg: &Config,
    tid: TargetId,
    build_dir: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut out = Vec::new();
    for t in graph.link_closure(tid) {
        let target = sess.target(t);
        for block in [&target.public, &target.private] {
            let Some(v) = block.get("link_flags") else { continue };
            let cfg = cfg.for_package(&sess.package(target.package).name);
            let Some(v) = dowel_eval::specialize(v, &cfg) else { continue };
            for item in flatten(&v) {
                // 道は絶対パスへ展開する。リンクの作業ディレクトリは
                // ビルドディレクトリであり、パッケージの中のリンカスクリプトを
                // 相対で指しても届かない（issue #70）。
                if let Some(abs) = absolute_path(sess, &item, build_dir, diags) {
                    out.push(abs.display().to_string());
                } else if let Some(s) = item.as_str() {
                    out.push(s.to_string());
                }
            }
        }
    }
    out
}

fn collect_sources(
    sess: &Session,
    tid: TargetId,
    cfg: &Config,
    diags: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let target = sess.target(tid);
    let pkg_root = sess.package(target.package).root.clone();
    let Some(value) = target.root.get("sources") else { return Vec::new() };
    // 具体化は宣言したパッケージで行う（ADR-0017）。
    let cfg = cfg.for_package(&sess.package(target.package).name);
    let Some(value) = dowel_eval::specialize(value, &cfg) else { return Vec::new() };

    let mut out = Vec::new();
    for item in flatten(&value) {
        match &item.data {
            Data::Glob(pattern) => {
                let hits = glob::expand(&pkg_root, pattern);
                if hits.is_empty() {
                    let mut d = Diagnostic::warning(
                        "empty-glob",
                        format!("`glob({pattern:?})` matched no files"),
                    );
                    if let Some(s) = item.prov.nearest_site() {
                        d = d.at(s.file, s.span, "no matches");
                    }
                    diags.push(d.note(format!("scanned {}", pkg_root.display())));
                }
                out.extend(hits.into_iter().map(|rel| pkg_root.join(rel)));
            }
            Data::Path(p) if p.base == PathBase::Package => {
                // 明示されたソースは、ここで実在を確かめる。
                // 通さなければ、無いファイルはビルドツールの「no known rule」に、
                // ディレクトリはリンカの「input file unused」になる。
                // どちらもマニフェストのどの行が原因かを示さない。
                let path = pkg_root.join(&p.rel);
                let site = item.prov.nearest_site();
                match std::fs::metadata(&path) {
                    Ok(m) if m.is_dir() => {
                        let mut d = Diagnostic::error(
                            "invalid-source",
                            format!("`{}` is a directory, not a source file", p.rel),
                        );
                        if let Some(s) = site {
                            d = d.at(s.file, s.span, "a directory cannot be compiled");
                        }
                        diags.push(d.note(format!(
                            "use `glob(\"{}/*.c\")` to take the files inside it",
                            p.rel
                        )));
                    }
                    Ok(_) => out.push(path),
                    Err(e) => {
                        let mut d = Diagnostic::error(
                            "unresolved-path",
                            format!("cannot read `{}`: {e}", p.rel),
                        );
                        if let Some(s) = site {
                            d = d.at(s.file, s.span, "declared here");
                        }
                        diags.push(d.note(format!("looked in {}", pkg_root.display())));
                    }
                }
            }
            Data::Error => {}
            _ => {
                let mut d = Diagnostic::error(
                    "invalid-source",
                    format!("element of `sources` is not a path: {}", item.display()),
                );
                if let Some(s) = item.prov.nearest_site() {
                    d = d.at(s.file, s.span, "expected a path");
                }
                diags.push(d);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 伝播してきた `Path` を絶対パスにする。
///
/// 基準点は値ではなく宣言された位置が決める。`libfoo` の `dir("include")` は
/// `libfoo` のルートから解決されなければならない。
fn collect_includes(
    sess: &Session,
    env: &dowel_model::PropMap,
    build_dir: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let Some(value) = env.get("includes") else { return Vec::new() };
    let mut out = Vec::new();
    for item in flatten(value) {
        let Some(abs) = absolute_path(sess, &item, build_dir, diags) else { continue };
        if !out.contains(&abs) {
            out.push(abs);
        }
    }
    out
}

/// `Path` の値を絶対パスにする。`Path` でなければ `None`。
///
/// 基準点は値ではなく宣言された位置が決める。`libfoo` の `dir("include")` は
/// `libfoo` のルートから解決されなければならない。解決できない基準は診断する。
fn absolute_path(
    sess: &Session,
    item: &Value,
    build_dir: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Option<PathBuf> {
    let Data::Path(p) = &item.data else { return None };
    let base = match p.base {
        PathBase::Package => {
            let pkg = item
                .prov
                .nearest_site()
                .and_then(|s| sess.package_of_file(s.file))
                .map(|id| sess.package(id).root.clone());
            match pkg {
                Some(root) => root,
                None => {
                    diags.push(Diagnostic::error(
                        "unresolved-path",
                        format!("cannot determine the base of `{}`", p.rel),
                    ));
                    return None;
                }
            }
        }
        PathBase::BuildDir => build_dir.to_path_buf(),
        PathBase::Sysroot => {
            diags.push(
                Diagnostic::error(
                    "unimplemented-path-base",
                    "sysroot-relative paths are not implemented",
                )
                .note("toolchain descriptions are Phase 5 (docs/90-roadmap.md)"),
            );
            return None;
        }
    };
    Some(base.join(&p.rel))
}

fn collect_defines(env: &dowel_model::PropMap) -> Vec<(String, String)> {
    let Some(value) = env.get("defines") else { return Vec::new() };
    let Some(map) = value.as_map() else { return Vec::new() };
    map.iter()
        .map(|(k, v)| {
            let rendered = match &v.data {
                Data::Str(s) => s.clone(),
                Data::Int(i) => i.to_string(),
                Data::Bool(b) => if *b { "1" } else { "0" }.to_string(),
                _ => String::new(),
            };
            (k.clone(), rendered)
        })
        .collect()
}

/// `c_std` / `cxx_std` から `-std=...` を1つ組み立てる。
///
/// 併合は `max` であり、閉包の中で最も高い標準が既に選ばれている
/// （`dowel_eval::schema::Merge::Max`）。C++17 を要求するライブラリを
/// C++20 の実行ファイルから使う形が、そのまま通る。
fn std_flag(env: &dowel_model::PropMap, name: &str) -> Option<String> {
    env.get(name).and_then(|v| v.as_str()).map(|s| format!("-std={s}"))
}

fn collect_flags(env: &dowel_model::PropMap, name: &str) -> Vec<String> {
    let Some(value) = env.get(name) else { return Vec::new() };
    flatten_strs(value)
}

/// 具体化済みの値から文字列の列を取り出す。入れ子は最後まで解く。
///
/// 引数の列（`args`）を読む箇所が計画の外にもあるため公開する
/// （`dowel inspect` は計画に載らない、issue #60）。
pub fn flatten_strs(value: &Value) -> Vec<String> {
    flatten(value).iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
}

/// 列の値なら要素へ、そうでなければ自身を1要素として返す。入れ子は最後まで解く。
/// 理由は `dowel_eval::schema` の同名関数に記した。
fn flatten(value: &Value) -> Vec<Value> {
    match &value.data {
        Data::List(items) => items.iter().flat_map(flatten).collect(),
        Data::Error => Vec::new(),
        _ => vec![value.clone()],
    }
}

fn object_path(build_dir: &Path, pkg: &str, target: &str, pkg_root: &Path, src: &Path) -> PathBuf {
    let rel = src.strip_prefix(pkg_root).unwrap_or(src);
    // パッケージ外のソースでも衝突しないよう、区切りを潰した名前にする。
    let flat = rel.to_string_lossy().replace(['/', '\\', ':'], "_");
    build_dir.join("obj").join(pkg).join(target).join(format!("{flat}.o"))
}

fn rel_display(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}
