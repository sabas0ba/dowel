//! `dowel` コマンド。
//!
//! 出力の分担を一貫させる。
//!
//! - **stdout** — 成果物。JSON 診断、グラフ、スキーマ、`why` の結果
//! - **stderr** — 進行と診断の人間向け表示、ログ
//!
//! これにより `dowel graph --format=dot | dot -Tsvg` がログ水準に関わらず動く。

mod args;

use args::{Command, GraphKind, MessageFormat, Options, OutFormat, Parsed};
use dowel_build::{compdb, exec, plan as build_plan, testing};
use dowel_eval::schema::{self, Block};
use dowel_eval::{Config, Opt};
use dowel_model::{graph, interface, package, Session};
use dowel_support::json::JsonWriter;
use dowel_support::{diag, log, log_debug, log_info, Severity};
use std::io::Write;
use std::process::ExitCode;

/// 使い方の誤り。診断による失敗（1）と区別する。
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let opts = match args::parse(argv) {
        Ok(Parsed::Help) => {
            print!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("dowel {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(o)) => *o,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("run `dowel --help` for usage");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    log::init(opts.log_level, opts.log_format, opts.color);
    log_debug!("starting dowel {}", env!("CARGO_PKG_VERSION"));

    match run(&opts) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn run(opts: &Options) -> Result<ExitCode, String> {
    if opts.command == Command::SchemaDump {
        // スキーマの出力はマニフェストを要さない。
        println!("{}", schema_dump());
        return Ok(ExitCode::SUCCESS);
    }

    let mut sess = Session::load(&opts.directory);
    let cfg = configure(&sess, opts)?;
    log_debug!("configuration {}", cfg.id());

    // グラフとインタフェースの診断も検査の一部。ここまでは常に走らせる。
    let (g, gdiags) = graph::build(&sess, &cfg);
    sess.diagnostics.extend(gdiags);
    let (ifaces, idiags) = interface::compute(&sess, &g, &cfg);
    sess.diagnostics.extend(idiags);

    match &opts.command {
        Command::SchemaDump => unreachable!("handled above"),

        Command::Check => {
            // 併合の診断（衝突・ABI 不一致）は compile_env を求めて初めて出る。
            let mut diags = Vec::new();
            for t in &sess.targets {
                interface::compile_env(&sess, &g, &ifaces, t.id, &cfg, &mut diags);
            }
            sess.diagnostics.extend(diags);
            let failed = report(&sess, opts);
            if !failed {
                eprintln!(
                    "check passed: {} packages, {} targets",
                    sess.packages.len(),
                    sess.targets.len()
                );
            }
            Ok(exit_code(failed))
        }

        Command::Graph => {
            if report(&sess, opts) {
                return Ok(ExitCode::FAILURE);
            }
            let text = match opts.graph_kind {
                GraphKind::Target => match opts.out_format {
                    OutFormat::Text => dowel_model::dump::text(&sess, &g),
                    OutFormat::Dot => dowel_model::dump::dot(&sess, &g),
                    OutFormat::Json => dowel_model::dump::json(&sess, &g),
                },
                GraphKind::Action => {
                    let requested = default_targets(&sess, &[])?;
                    let (p, pdiags) = build_plan::plan(&sess, &g, &ifaces, &cfg, &requested);
                    sess.diagnostics.extend(pdiags);
                    if report(&sess, opts) {
                        return Ok(ExitCode::FAILURE);
                    }
                    match opts.out_format {
                        OutFormat::Text => dowel_build::dump::text(&sess, &p),
                        OutFormat::Dot => dowel_build::dump::dot(&sess, &p),
                        OutFormat::Json => dowel_build::dump::json(&sess, &p),
                    }
                }
            };
            print!("{text}");
            Ok(ExitCode::SUCCESS)
        }

        Command::Why { target, property } => {
            if report(&sess, opts) {
                return Ok(ExitCode::FAILURE);
            }
            let tid = sess.find_target(target)?;
            let e = dowel_model::why::explain(&sess, &g, &ifaces, tid, property, &cfg)?;
            match opts.out_format {
                OutFormat::Json => println!("{}", dowel_model::why::render_json(&e)),
                _ => print!("{}", dowel_model::why::render_text(&e)),
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Build { targets } => {
            let requested = default_targets(&sess, targets)?;
            let Some(p) = build(&mut sess, &g, &ifaces, &cfg, opts, &requested)? else {
                return Ok(ExitCode::FAILURE);
            };
            for t in &requested {
                if let Some(path) = p.artifacts.get(t) {
                    eprintln!("built: {}", path.display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Test { targets } => {
            let requested = test_targets(&sess, targets)?;
            if requested.is_empty() {
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
                eprintln!("no test targets. declare one with `[test.<name>]` in dowel.build");
                return Ok(ExitCode::SUCCESS);
            }
            let Some(p) = build(&mut sess, &g, &ifaces, &cfg, opts, &requested)? else {
                return Ok(ExitCode::FAILURE);
            };
            if opts.no_run {
                for t in &requested {
                    if let Some(path) = p.artifacts.get(t) {
                        eprintln!("built: {}", path.display());
                    }
                }
                return Ok(ExitCode::SUCCESS);
            }

            let launcher = testing::Launcher::for_config(&cfg);
            let outcomes = testing::run(&sess, &p, &launcher, &requested, !opts.nocapture);
            Ok(report_tests(&outcomes, opts))
        }
    }
}

/// 計画してビルドする。`build` と `test` で共通の経路。
///
/// 診断か実行で失敗した場合は報告済みの `None` を返す。
fn build(
    sess: &mut Session,
    g: &dowel_model::Graph,
    ifaces: &dowel_model::Interfaces,
    cfg: &Config,
    opts: &Options,
    requested: &[dowel_model::TargetId],
) -> Result<Option<build_plan::Plan>, String> {
    let (p, pdiags) = build_plan::plan(sess, g, ifaces, cfg, requested);
    sess.diagnostics.extend(pdiags);
    if report(sess, opts) {
        return Ok(None);
    }

    if opts.compdb {
        let root = sess.root_package().map(|p| p.root.clone()).unwrap_or_default();
        match compdb::write(&p, &root) {
            Ok(paths) => {
                for path in paths {
                    log_info!("wrote {}", path.display());
                }
            }
            Err(e) => eprintln!("warning: cannot write compile_commands.json: {e}"),
        }
    }

    let executor = choose_executor(opts)?;
    log_debug!("executor {executor:?}");
    if let Err(f) = exec::run(&p, executor, opts.jobs) {
        eprint!("error: {f}");
        return Ok(None);
    }
    Ok(Some(p))
}

/// テストの結果を報告する。誤りが1件でもあれば非零で終わる。
///
/// 出力の分担は他のコマンドと同じ。機械可読な結果は stdout、
/// 進行と要約は stderr。
fn report_tests(outcomes: &[testing::Outcome], opts: &Options) -> ExitCode {
    let passed = outcomes.iter().filter(|o| o.passed).count();
    let failed = outcomes.len() - passed;

    if opts.message_format == MessageFormat::Json {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for o in outcomes {
            let _ = writeln!(out, "{}", testing::render_json(o));
        }
    }

    eprintln!("running {} test{}", outcomes.len(), if outcomes.len() == 1 { "" } else { "s" });
    for o in outcomes {
        eprintln!("{}", o.summary_line());
    }

    if failed > 0 {
        eprintln!("\nfailures:");
        for o in outcomes.iter().filter(|o| !o.passed) {
            eprintln!("\n---- {} ----", o.label);
            if let Some(reason) = o.failure_reason() {
                eprintln!("{reason}");
            }
            // 出力は失敗したものだけ見せる。通ったテストの出力は雑音になる。
            if !o.stdout.trim().is_empty() {
                eprintln!("--- stdout ---\n{}", o.stdout.trim_end());
            }
            if !o.stderr.trim().is_empty() {
                eprintln!("--- stderr ---\n{}", o.stderr.trim_end());
            }
        }
    }

    eprintln!(
        "\ntest result: {}. {passed} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" }
    );
    exit_code(failed > 0)
}

/// 構成を組み立てる。機能フラグは根のパッケージの `[features]` から解決する。
fn configure(sess: &Session, opts: &Options) -> Result<Config, String> {
    let mut cfg = Config::host_default();
    cfg.opt = Opt::parse(&opts.config)
        .ok_or_else(|| format!("`--config` must be debug or release (got `{}`)", opts.config))?;
    if let Some(t) = &opts.target {
        cfg.target = t.clone();
    }
    if let Some(root) = sess.root_package() {
        cfg.features = package::resolve_features(root, &opts.features, opts.default_features);
        if let Some(tc) = &root.toolchain_c {
            cfg.tc_c = tc.clone();
        }
    }
    Ok(cfg)
}

/// 対象の決定。指定がなければ全ての bin と test。
fn default_targets(
    sess: &Session,
    requested: &[String],
) -> Result<Vec<dowel_model::TargetId>, String> {
    if !requested.is_empty() {
        return requested.iter().map(|s| sess.find_target(s)).collect();
    }
    use dowel_eval::schema::TableKind;
    let out: Vec<_> = sess
        .targets
        .iter()
        .filter(|t| matches!(t.kind, TableKind::Bin | TableKind::Test))
        .map(|t| t.id)
        .collect();
    if out.is_empty() {
        // ライブラリしかない場合はそれを作る。
        return Ok(sess.targets.iter().map(|t| t.id).collect());
    }
    Ok(out)
}

/// `test` の対象。指定がなければ全ての test ターゲット。
fn test_targets(
    sess: &Session,
    requested: &[String],
) -> Result<Vec<dowel_model::TargetId>, String> {
    use dowel_eval::schema::TableKind;
    if !requested.is_empty() {
        let ids: Vec<_> =
            requested.iter().map(|s| sess.find_target(s)).collect::<Result<_, _>>()?;
        // 明示指定が test 以外なら、黙って走らせずに断る。
        for id in &ids {
            let t = sess.target(*id);
            if t.kind != TableKind::Test {
                return Err(format!(
                    "`{}` is a {} target, not a test",
                    sess.label(*id),
                    t.kind.name()
                ));
            }
        }
        return Ok(ids);
    }
    Ok(sess.targets.iter().filter(|t| t.kind == TableKind::Test).map(|t| t.id).collect())
}

fn choose_executor(opts: &Options) -> Result<exec::Executor, String> {
    match &opts.executor {
        Some(s) => exec::Executor::parse(s)
            .ok_or_else(|| format!("`--executor` must be ninja or direct (got `{s}`)")),
        None => Ok(if exec::ninja_available() {
            exec::Executor::Ninja
        } else {
            log_debug!("ninja not found; falling back to the direct executor");
            exec::Executor::Direct
        }),
    }
}

/// 診断を出力する。誤りが1件でもあれば `true`。
fn report(sess: &Session, opts: &Options) -> bool {
    let errors = sess.diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    let warnings = sess.diagnostics.iter().filter(|d| d.severity == Severity::Warning).count();

    match opts.message_format {
        MessageFormat::Json => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            for d in &sess.diagnostics {
                let _ = writeln!(out, "{}", diag::render_json(d, &sess.sm));
            }
        }
        MessageFormat::Human => {
            for d in &sess.diagnostics {
                eprint!("{}", diag::render(d, &sess.sm, opts.color));
            }
            if errors > 0 || warnings > 0 {
                eprintln!("{errors} errors, {warnings} warnings");
            }
        }
    }
    errors > 0
}

fn exit_code(failed: bool) -> ExitCode {
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// スキーマと構成語彙の機械可読な出力。
///
/// コーパス不在を補うためにエージェントへ渡す前提の出力である
/// （docs/30-devexp.md 4節）。
fn schema_dump() -> String {
    use dowel_eval::config::{Domain, VOCABULARY};
    let mut w = JsonWriter::pretty();
    w.begin_object();

    w.key("table_kinds").begin_array();
    for k in schema::TableKind::ALL {
        w.begin_object();
        w.field_str("name", k.name());
        w.field_bool("is_target", k.is_target());
        w.field_bool("implemented", k.is_implemented());
        w.end_object();
    }
    w.end_array();

    w.key("blocks").begin_array();
    for (block, name) in
        [(Block::Root, "(root)"), (Block::Public, "public"), (Block::Private, "private")]
    {
        w.begin_object();
        w.field_str("name", name);
        w.field_bool("propagates", block == Block::Public);
        w.key("properties").begin_array();
        let props = if block == Block::Root { schema::root_props() } else { schema::block_props() };
        for p in props {
            w.begin_object();
            w.field_str("name", p.name);
            w.field_str("type", &p.ty.display());
            w.field_str("merge", p.merge.name());
            w.field_str("doc", p.doc);
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();

    w.key("functions").begin_array();
    for (name, sig, doc) in [
        ("glob", "(Str) -> List<Path>", "files matching the pattern; expanded at plan time"),
        ("dir", "(Str) -> Path", "a directory relative to the package root"),
        ("file", "(Str) -> Path", "a file relative to the package root"),
        ("dep", "(Str) -> DepRef", "a reference to a dependency declared in dowel.toml"),
        ("target", "(Str) -> TargetRef", "a reference to a target in the same package"),
    ] {
        w.begin_object();
        w.field_str("name", name);
        w.field_str("signature", sig);
        w.field_str("doc", doc);
        w.end_object();
    }
    w.end_array();

    w.key("cfg").begin_object();
    w.field_str("status", "provisional; under discussion as Q1 in docs/99-open-questions.md");
    w.key("keys").begin_array();
    for (ns, name, domain, doc) in VOCABULARY {
        w.begin_object();
        w.field_str("name", &format!("{ns}.{name}"));
        w.field_str("doc", doc);
        match domain {
            Domain::Finite(values) => {
                w.field_str("domain", "finite");
                w.field_strs("values", values.iter().copied());
            }
            Domain::Bool => {
                w.field_str("domain", "bool");
            }
            Domain::Open => {
                w.field_str("domain", "open");
                // 値域が開いているキーは `match` で `_` を要求する。
                w.field_bool("requires_wildcard", true);
            }
        }
        w.end_object();
    }
    w.end_array();
    w.end_object();

    w.end_object();
    w.finish()
}
