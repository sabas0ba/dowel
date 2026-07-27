//! アクショングラフの構築。
//!
//! ここが「構成を与えて具体化する」唯一の場所である。マニフェスト評価は
//! 構成を知らず、`Cfg<T>` のまま値を持っている（docs/10-manifest.md 3節）。

use crate::action::{Action, ActionId, ActionKind};
use crate::glob;
use dowel_eval::schema::TableKind;
use dowel_eval::{Config, Data, Opt, PathBase, Value};
use dowel_model::graph::Graph;
use dowel_model::interface::{self, Interfaces};
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
    pub compile_commands: Vec<CompileCommand>,
    /// 要求されたターゲット
    pub requested: Vec<TargetId>,
}

impl Plan {
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
                "アクションの依存が後方を指している"
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
    ifaces: &Interfaces,
    cfg: &Config,
    requested: &[TargetId],
) -> (Plan, Vec<Diagnostic>) {
    let _phase = dowel_support::log::Phase::start("plan");
    let mut diags = Vec::new();
    let root = sess.root_package().map(|p| p.root.clone()).unwrap_or_else(|| PathBuf::from("."));
    let build_dir = build_dir(&root, cfg);

    // ツールチェーンが混ざると ABI の前提が崩れる。1回のビルドで1つに限る。
    for p in &sess.packages {
        if let Some(tc) = &p.toolchain_c {
            if *tc != cfg.tc_c {
                diags.push(
                    Diagnostic::warning(
                        "toolchain-mismatch",
                        format!(
                            "パッケージ `{}` は C ツールチェーンに `{tc}` を要求しているが、`{}` でビルドする",
                            p.name, cfg.tc_c
                        ),
                    )
                    .note("ツールチェーンの取得と切り替えは Phase 5（docs/90-roadmap.md）")
                    .note("ABI ラベルの検証はツールチェーンが固定されていることを前提にする"),
                );
            }
        }
    }

    // 必要なターゲットの集合。要求されたものとその推移的依存。
    let mut needed: BTreeSet<TargetId> = BTreeSet::new();
    for &t in requested {
        needed.extend(graph.link_closure(t));
    }

    let mut plan = Plan {
        build_dir: build_dir.clone(),
        actions: Vec::new(),
        artifacts: BTreeMap::new(),
        compile_commands: Vec::new(),
        requested: requested.to_vec(),
    };
    // ターゲット → そのターゲットの成果物を作るアクション
    let mut producer: BTreeMap<TargetId, ActionId> = BTreeMap::new();

    // `graph.order` は依存が先。成果物ができてからリンクする順になる。
    for &tid in &graph.order {
        if !needed.contains(&tid) {
            continue;
        }
        let target = sess.target(tid);
        let pkg = sess.package(target.package);
        let env = interface::compile_env(sess, graph, ifaces, tid, cfg, &mut diags);

        let sources = collect_sources(sess, tid, cfg, &mut diags);
        if sources.is_empty() && target.kind != TableKind::Lib {
            diags.push(
                Diagnostic::error("no-sources", format!("`{}` にソースがない", sess.label(tid)))
                    .at(target.site.file, target.site.span, "`sources` が空")
                    .note("`sources = glob(\"src/*.c\")` のように指定する"),
            );
            continue;
        }

        let includes = collect_includes(sess, &env, &build_dir, &mut diags);
        let defines = collect_defines(&env);
        let flags = collect_flags(&env, "flags");
        let link_flags = collect_flags(&env, "link_flags");

        log_trace!(
            "{}: ソース {} 件、include {} 件、define {} 件",
            sess.label(tid),
            sources.len(),
            includes.len(),
            defines.len()
        );

        // --- コンパイル ---
        let mut objects = Vec::new();
        let mut compile_ids = Vec::new();
        for src in &sources {
            let obj = object_path(&build_dir, &pkg.name, &target.name, &pkg.root, src);
            let depfile = obj.with_extension("o.d");
            let mut args: Vec<String> = Vec::new();
            args.extend(default_compile_flags(cfg));
            args.extend(flags.iter().cloned());
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
                program: cfg.tc_c.clone(),
                args: args.clone(),
                inputs: vec![src.clone()],
                outputs: vec![obj.clone()],
                depfile: Some(depfile),
                description: format!("CC {}", rel_display(&build_dir, &obj)),
                deps: Vec::new(),
            });
            let mut arguments = vec![cfg.tc_c.clone()];
            arguments.extend(args);
            plan.compile_commands.push(CompileCommand {
                directory: build_dir.clone(),
                file: src.clone(),
                arguments,
                output: obj.clone(),
            });
            objects.push(obj);
            compile_ids.push(id);
        }

        // --- 集約（アーカイブ／リンク） ---
        match target.kind {
            TableKind::Lib => {
                let out = build_dir.join("lib").join(format!("lib{}.a", target.name));
                let mut args = vec!["rcs".to_string(), out.display().to_string()];
                args.extend(objects.iter().map(|o| o.display().to_string()));
                let id = ActionId(plan.actions.len());
                plan.actions.push(Action {
                    id,
                    kind: ActionKind::Archive,
                    target: tid,
                    program: "ar".into(),
                    args,
                    inputs: objects.clone(),
                    outputs: vec![out.clone()],
                    depfile: None,
                    description: format!("AR {}", rel_display(&build_dir, &out)),
                    deps: compile_ids.clone(),
                });
                producer.insert(tid, id);
                plan.artifacts.insert(tid, out);
            }
            TableKind::Bin | TableKind::Test => {
                let out = build_dir.join("bin").join(&target.name);
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
                    program: cfg.tc_c.clone(),
                    args,
                    inputs,
                    outputs: vec![out.clone()],
                    depfile: None,
                    description: format!("LINK {}", rel_display(&build_dir, &out)),
                    deps,
                });
                producer.insert(tid, id);
                plan.artifacts.insert(tid, out);
            }
            other => diags.push(
                Diagnostic::error(
                    "unimplemented-kind",
                    format!("`{}` の成果物は作れない", other.name()),
                )
                .at(target.site.file, target.site.span, "未実装の種別"),
            ),
        }
    }

    log_debug!(
        "アクション {} 件（コンパイル {}、アーカイブ {}、リンク {}）",
        plan.actions.len(),
        count(&plan, ActionKind::Compile),
        count(&plan, ActionKind::Archive),
        count(&plan, ActionKind::Link)
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

fn collect_sources(
    sess: &Session,
    tid: TargetId,
    cfg: &Config,
    diags: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let target = sess.target(tid);
    let pkg_root = sess.package(target.package).root.clone();
    let Some(value) = target.root.get("sources") else { return Vec::new() };
    let Some(value) = dowel_eval::specialize(value, cfg) else { return Vec::new() };

    let mut out = Vec::new();
    for item in flatten(&value) {
        match &item.data {
            Data::Glob(pattern) => {
                let hits = glob::expand(&pkg_root, pattern);
                if hits.is_empty() {
                    let mut d = Diagnostic::warning(
                        "empty-glob",
                        format!("`glob({pattern:?})` に一致するファイルがない"),
                    );
                    if let Some(s) = item.prov.nearest_site() {
                        d = d.at(s.file, s.span, "一致なし");
                    }
                    diags.push(d.note(format!("走査したのは {}", pkg_root.display())));
                }
                out.extend(hits.into_iter().map(|rel| pkg_root.join(rel)));
            }
            Data::Path(p) if p.base == PathBase::Package => {
                out.push(pkg_root.join(&p.rel));
            }
            Data::Error => {}
            _ => {
                let mut d = Diagnostic::error(
                    "invalid-source",
                    format!("`sources` の要素がパスでない: {}", item.display()),
                );
                if let Some(s) = item.prov.nearest_site() {
                    d = d.at(s.file, s.span, "パスが必要");
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
/// 基準点は値ではなく**宣言された位置**が決める。`libfoo` の `dir("include")` は
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
        let Data::Path(p) = &item.data else { continue };
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
                            format!("`{}` の基準点を決められない", p.rel),
                        ));
                        continue;
                    }
                }
            }
            PathBase::BuildDir => build_dir.to_path_buf(),
            PathBase::Sysroot => {
                diags.push(
                    Diagnostic::error("unimplemented-path-base", "sysroot 基準のパスは未実装")
                        .note("ツールチェーン記述は Phase 5（docs/90-roadmap.md）"),
                );
                continue;
            }
        };
        let abs = base.join(&p.rel);
        if !out.contains(&abs) {
            out.push(abs);
        }
    }
    out
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

fn collect_flags(env: &dowel_model::PropMap, name: &str) -> Vec<String> {
    let Some(value) = env.get(name) else { return Vec::new() };
    flatten(value).iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
}

fn flatten(value: &Value) -> Vec<Value> {
    match &value.data {
        Data::List(items) => items.clone(),
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
