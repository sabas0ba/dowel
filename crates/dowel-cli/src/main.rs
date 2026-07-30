//! `dowel` コマンド。
//!
//! 出力の分担を一貫させる。
//!
//! - **stdout** — 成果物。JSON 診断、グラフ、スキーマ、`why` の結果
//! - **stderr** — 進行と診断の人間向け表示、ログ
//!
//! これにより `dowel graph --format=dot | dot -Tsvg` がログ水準に関わらず動く。

mod args;
mod import;
mod scaffold;

use args::{Command, GraphKind, MessageFormat, Options, OutFormat, Parsed};
use dowel_build::{compdb, exec, plan as build_plan, testing};
use dowel_eval::schema::{self, Block};
use dowel_eval::{Config, Opt};
use dowel_model::{graph, interface, Session};
use dowel_support::json::JsonWriter;
use dowel_support::{diag, log, log_debug, log_info, log_trace, Diagnostic, Severity};
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
    // ストアの操作もマニフェストを要さない。壊れたマニフェストの状態でも
    // 掃除できる必要がある。
    if opts.command == Command::CacheInfo {
        return cache_info(&opts.directory);
    }
    // 言語サーバはマニフェストを要さない。開いている緩衝が正本であり、
    // 起動時に読むものは無い（docs/30-devexp.md 3.2）。
    if opts.command == Command::Lsp {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        dowel_lsp::serve(&mut stdin.lock(), &mut stdout.lock())
            .map_err(|e| format!("the language server stopped: {e}"))?;
        return Ok(ExitCode::SUCCESS);
    }
    if opts.command == Command::CacheGc {
        let removed = dowel_store::Store::gc(&opts.directory)
            .map_err(|e| format!("cannot clean the store: {e}"))?;
        eprintln!("removed {removed} store(s) left by older formats");
        return Ok(ExitCode::SUCCESS);
    }
    // 下書きの生成はマニフェストを要さない。読むのは CMake の reply である。
    if let Command::MigrateImport { reply } = &opts.command {
        import::import(&opts.directory.join(reply))?;
        return Ok(ExitCode::SUCCESS);
    }
    // 雛型の生成もマニフェストを要さない（`add` は自分で読む）。
    if let Command::New { path } = &opts.command {
        scaffold::new_package(&opts.directory.join(path), opts.lib)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Command::Add { path } = &opts.command {
        match (&opts.git, path) {
            (Some(url), None) => scaffold::add_git_dependency(
                &opts.directory,
                url,
                opts.rev.as_deref(),
                opts.dep_name.as_deref(),
            )?,
            (None, Some(rel)) => {
                scaffold::add_package(&opts.directory, rel, opts.dep_name.as_deref())?
            }
            (Some(_), Some(_)) => {
                return Err("`add` takes either <path> or `--git <url>`, not both".into())
            }
            (None, None) => return Err("write `add <path>` or `add --git <url>`".into()),
        }
        return Ok(ExitCode::SUCCESS);
    }

    // 機能フラグの選択は読み込みより前に要る。有効でない任意の依存は
    // 読み込まないため（docs/10-manifest.md）。
    let mut sess = Session::load_with_max_nesting(
        &opts.directory,
        dowel_model::session::Features {
            requested: opts.features.clone(),
            default: opts.default_features,
        },
        opts.max_nesting,
    );
    // ストアへの書き込みは読み込み直後に行う。以降の段階が失敗しても、
    // 何を読み、どう評価したかは次回の実行にとって有効な情報である。
    sess.save();
    for (path, change) in sess.input_changes() {
        log_trace!("input {}: {change:?}", path.display());
    }
    let (cfg, cfg_diags) = configure(&sess, opts)?;
    sess.diagnostics.extend(cfg_diags);
    log_debug!("configuration {}", cfg.id());

    // グラフとインタフェースの診断も検査の一部。ここまでは常に走らせる。
    let (g, gdiags) = graph::build(&sess, &cfg);
    sess.diagnostics.extend(gdiags);
    let idiags = interface::prepare(&sess, &g, &cfg);
    sess.diagnostics.extend(idiags);

    match &opts.command {
        Command::SchemaDump
        | Command::CacheInfo
        | Command::CacheGc
        | Command::Lsp
        | Command::New { .. }
        | Command::Add { .. }
        | Command::MigrateImport { .. } => {
            unreachable!("handled above")
        }

        Command::Check => {
            // 計画まで走らせる。glob 展開・パス解決・ツールチェーンの実在は
            // 評価では判定できず（docs/10-manifest.md 3節）、ここを外すと
            // `check passed` と表示したものが `build` で落ちる。
            // アクションは生成するだけで実行せず、何も書かない。
            //
            // 併合の診断（衝突・ABI 不一致）も compile_env を経由して出る。
            // 対象は全ターゲット。到達しないライブラリも検査の対象である。
            let all: Vec<dowel_model::TargetId> = sess.targets.iter().map(|t| t.id).collect();
            let (_, pdiags) = build_plan::plan(&sess, &g, &cfg, &all);
            sess.diagnostics.extend(pdiags);
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

        Command::MigrateVerify { reference } => {
            // 参照は既存システムの compile_commands.json。dowel の計画を
            // 全ターゲットで立て、正規化した引数を突き合わせる
            // （docs/40-migration.md 4節）。未移植は途中経過であり失敗にしない。
            let text = std::fs::read_to_string(reference)
                .map_err(|e| format!("cannot read the reference `{reference}`: {e}"))?;
            let entries = dowel_build::migrate::read_reference(&text)
                .map_err(|e| format!("cannot use `{reference}`: {e}"))?;
            let all: Vec<dowel_model::TargetId> = sess.targets.iter().map(|t| t.id).collect();
            let (p, pdiags) = build_plan::plan(&sess, &g, &cfg, &all);
            sess.diagnostics.extend(pdiags);
            if report(&sess, opts) {
                return Ok(ExitCode::FAILURE);
            }
            let verdict = dowel_build::migrate::compare(&p, &entries);
            match opts.out_format {
                OutFormat::Json => println!("{}", dowel_build::migrate::render_json(&verdict)),
                _ => print!("{}", dowel_build::migrate::render_text(&verdict)),
            }
            Ok(exit_code(!verdict.ported_sources_are_equivalent()))
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
                    let (p, pdiags) = build_plan::plan(&sess, &g, &cfg, &requested);
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
            let e = dowel_model::why::explain(&sess, &g, tid, property, &cfg)?;
            match opts.out_format {
                OutFormat::Json => println!("{}", dowel_model::why::render_json(&e)),
                _ => print!("{}", dowel_model::why::render_text(&e)),
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Build { targets } => {
            let requested = default_targets(&sess, targets)?;
            let Some(p) = build(&mut sess, &g, &cfg, opts, &requested)? else {
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
            let mut requested = test_targets(&sess, targets)?;
            let build_dir = build_plan::build_dir(
                &sess.root_package().map(|p| p.root.clone()).unwrap_or_default(),
                &cfg,
            );
            let mut state = testing::State::load(&build_dir);

            if opts.only_failed {
                // 前回の判定はラベルで持つ。今あるターゲットとの突き合わせに失敗した
                // ものは黙って落とす（マニフェストから消えた場合）。
                let failed = state.failed();
                requested.retain(|t| failed.contains(&sess.label(*t).as_str()));
                if requested.is_empty() {
                    if report(&sess, opts) {
                        return Ok(ExitCode::FAILURE);
                    }
                    eprintln!(
                        "nothing to rerun. no failing tests were recorded in {}",
                        build_dir.display()
                    );
                    return Ok(ExitCode::SUCCESS);
                }
            }

            if requested.is_empty() {
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
                eprintln!("no test targets. declare one with `[test.<name>]` in dowel.build");
                return Ok(ExitCode::SUCCESS);
            }
            let Some(p) = build(&mut sess, &g, &cfg, opts, &requested)? else {
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

            // ランナーの解決は起動の直前ではなく、この位置で行って診断を出す。
            // クロス構成でランナーが無いまま起動すると `Exec format error` になり、
            // 構成の誤りがテストの失敗として報告される。
            let (launcher, runner_diags) = testing::Launcher::for_config(&sess, &cfg);
            if !runner_diags.is_empty() {
                sess.diagnostics.extend(runner_diags);
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
            }
            let run_opts = test_run_options(opts);
            let outcomes = testing::run(&sess, &p, &launcher, &requested, &run_opts);

            state.update(&outcomes);
            if let Err(e) = state.save(&p.build_dir) {
                eprintln!("warning: cannot record the test results: {e}");
            }
            Ok(report_tests(&outcomes, requested.len(), opts))
        }
    }
}

/// 計画してビルドする。`build` と `test` で共通の経路。
///
/// 診断か実行で失敗した場合は報告済みの `None` を返す。
fn build(
    sess: &mut Session,
    g: &dowel_model::Graph,
    cfg: &Config,
    opts: &Options,
    requested: &[dowel_model::TargetId],
) -> Result<Option<build_plan::Plan>, String> {
    let (p, pdiags) = build_plan::plan(sess, g, cfg, requested);
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

/// 実行のしかたを組み立てる。
fn test_run_options(opts: &Options) -> testing::RunOptions {
    let capture = !opts.nocapture;
    let mut jobs = opts.test_jobs.unwrap_or(1).max(1);
    if !capture && jobs > 1 {
        // 素通しでの並列は出力が混ざり、読めるものにならない。
        eprintln!("note: `--nocapture` forces one test at a time");
        jobs = 1;
    }
    testing::RunOptions { capture, fail_fast: opts.fail_fast, jobs }
}

/// テストの結果を報告する。誤りが1件でもあれば非零で終わる。
///
/// 出力の分担は他のコマンドと同じ。機械可読な結果は stdout、
/// 進行と要約は stderr。
fn report_tests(outcomes: &[testing::Outcome], requested: usize, opts: &Options) -> ExitCode {
    let passed = outcomes.iter().filter(|o| o.passed).count();
    let failed = outcomes.len() - passed;
    let not_run = requested.saturating_sub(outcomes.len());

    if opts.message_format == MessageFormat::Json {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for o in outcomes {
            let _ = writeln!(out, "{}", testing::render_json(o));
        }
    }

    eprintln!("running {} test{}", requested, if requested == 1 { "" } else { "s" });
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

    // 打ち切った場合、走らせていない分を隠さない。
    let tail = if not_run > 0 { format!("; {not_run} not run") } else { String::new() };
    eprintln!(
        "\ntest result: {}. {passed} passed; {failed} failed{tail}",
        if failed == 0 { "ok" } else { "FAILED" }
    );
    exit_code(failed > 0)
}

/// 構成を組み立てる。機能フラグは根のパッケージの `[features]` から解決する。
///
/// `--features` に渡された名前も `[features]` の宣言に照らす。オプション解析の
/// 段では判定できない。値の妥当性は別の語彙（マニフェスト）が決めるものであり、
/// 引数解析にはその情報がない。
fn configure(sess: &Session, opts: &Options) -> Result<(Config, Vec<Diagnostic>), String> {
    let mut cfg = Config::host_default();
    let mut diags = Vec::new();
    cfg.opt = Opt::parse(&opts.config)
        .ok_or_else(|| format!("`--config` must be debug or release (got `{}`)", opts.config))?;
    if let Some(t) = &opts.target {
        cfg.target = t.clone();
    }
    if let Some(root) = sess.root_package() {
        let declared: Vec<String> = root.features.keys().cloned().collect();
        for name in &opts.features {
            if !declared.contains(name) {
                // 位置は `[features]` の見出しを指す。誤りは `--features` に
                // あるが、正しい綴りが書かれている場所はそこである。
                diags.push(
                    dowel_model::session::unknown_feature(
                        name,
                        &declared,
                        root.features_site,
                        "declared features are here",
                    )
                    .note(format!("`{name}` came from `--features`")),
                );
            }
        }
        // 読み込みの段で解決した集合をそのまま使う。二重に求めると、
        // 「読み込んだ依存」と「有効な機能」が食い違いうる。
        cfg.features = sess.active_features().clone();

        // ツールチェーンはターゲットトリプルで選ぶ。`[runner.<triple>]` と
        // 同じ形である。宣言の無いトリプルはここで拒む。ホストのコンパイラで
        // 組んで別トリプルの名前を付けると、誤りが qemu の
        // `Invalid ELF image` などとして1段あとに現れる（issue #42）。
        let host = dowel_eval::config::default_triple();
        match root.toolchain_for(&cfg.target, &host) {
            Some(decl) => {
                if let Some(tc) = &decl.c {
                    cfg.tc_c = tc.clone();
                }
                if let Some(tc) = &decl.cxx {
                    cfg.tc_cxx = tc.clone();
                }
            }
            None => {
                let declared: Vec<&str> = root.toolchains.keys().map(|s| s.as_str()).collect();
                let mut d = Diagnostic::error(
                    "missing-toolchain",
                    format!("no toolchain is declared for target `{}`", cfg.target),
                )
                .note("building with the host toolchain would produce artifacts for the wrong architecture under this target's name")
                .note(format!(
                    "declare one, for example `[toolchain.{}]` with `c = \"...\"` in dowel.toml",
                    cfg.target
                ));
                if !declared.is_empty() {
                    d = d.note(format!("toolchains are declared for: {}", declared.join(", ")));
                }
                diags.push(d);
            }
        }
    }
    Ok((cfg, diags))
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

/// ストアの規模を報告する。stdout へ出すのは成果物であるため。
fn cache_info(root: &std::path::Path) -> Result<ExitCode, String> {
    let dir = dowel_store::Store::dir(root);
    let store = dowel_store::Store::open(root);
    let values = std::fs::metadata(dir.join("values")).map(|m| m.len()).unwrap_or(0);
    println!("directory  {}", dir.display());
    println!("format     {}", dowel_store::FORMAT);
    println!("records    {}", store.len());
    println!("values     {values} bytes");
    Ok(ExitCode::SUCCESS)
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
    for (name, sig, doc) in schema::FUNCTIONS {
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
