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
use dowel_build::{backend, compdb, plan as build_plan, testing, BuildGraph};
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

    // 道具について確かめたことは、プロジェクトを跨いで憶えておく（ADR-0028）。
    // 作るのはここ1つで、書き出すのも戻ってきてから1度だけ——`run` は
    // 途中で幾つも返るので、内側に置くと保存を書き落とす。
    let mut probe = dowel_build::probe::Prober::new();
    let code = match run(&opts, &mut probe) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_USAGE)
        }
    };
    probe.save();
    code
}

fn run(opts: &Options, probe: &mut dowel_build::probe::Prober) -> Result<ExitCode, String> {
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
        // 事実も古い形式版を残す。片方だけ掃除すると、掃除したつもりの
        // 利用者に残骸が残る（ADR-0028）。
        let facts = dowel_store::facts::Facts::gc()
            .map_err(|e| format!("cannot clean the fact database: {e}"))?;
        eprintln!("removed {removed} store(s) and {facts} fact database(s) left by older formats");
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
    let (cfg, cfg_diags) = configure(&sess, opts, probe)?;
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
                    let requested = default_targets(&sess, &cfg, &[])?;
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
            let requested = default_targets(&sess, &cfg, targets)?;
            let backend = backend::select(opts.backend.as_deref(), probe)?;
            if !backend.builds() {
                return emit_only(&mut sess, &g, &cfg, opts, &requested, &*backend);
            }
            let Some(p) = build(&mut sess, &g, &cfg, opts, &requested, &*backend)? else {
                return Ok(ExitCode::FAILURE);
            };
            // 派生した成果物（`artifacts` ブロック）も作ったものとして述べる。
            // 述べないと、`.bin` が出来ていることが利用者に見えない。
            for path in p.default_outputs() {
                eprintln!("built: {}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Inspect { targets } => {
            // 検査は成果物を作らない。作らないため増分の対象にならず、
            // `build` の既定にも入らない——最新かどうかを判定する出力が無い。
            // 走らせるのは明示のこのコマンドである（issue #60）。
            let requested = inspect_targets(&sess, targets)?;
            if requested.is_empty() {
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
                eprintln!(
                    "no inspections. declare one with `[<kind>.<name>.inspect]` in dowel.build"
                );
                return Ok(ExitCode::SUCCESS);
            }
            let backend = building_backend(opts, "dowel inspect", probe)?;
            let Some(p) = build(&mut sess, &g, &cfg, opts, &requested, &*backend)? else {
                return Ok(ExitCode::FAILURE);
            };
            Ok(exit_code(inspect(&sess, &cfg, &p, &requested, opts)))
        }

        Command::Debug { target } => {
            // 位置引数は目標でも事例のラベルでもよい（issue #110）。
            // デバッガを開きたいのは失敗のときだけではない——通っているが
            // 遅い事例、これから書く事例、別の構成で落ちた事例。
            // `--debug-failed` は「前回落ちたものを開く」という別の選択であり、
            // どちらも要る。
            let (target_ref, case) = match target.split_once('/') {
                Some((t, c)) => (t, Some(c)),
                None => (target.as_str(), None),
            };
            let tid = sess.find_target(target_ref)?;
            let backend = building_backend(opts, "dowel debug", probe)?;
            let requested = vec![tid];
            let Some(p) = build(&mut sess, &g, &cfg, opts, &requested, &*backend)? else {
                return Ok(ExitCode::FAILURE);
            };
            // 事例を名指ししたなら、その宣言（args / env / cwd、ハーネスなら
            // `run` と名前）を運ぶ。数え上げは `dowel test` と同じ経路である。
            let launcher = testing::Launcher::for_config(&sess, &cfg).0;
            let job = match case {
                None => None,
                Some(name) => match find_case(&sess, &p, &cfg, &launcher, tid, name) {
                    Ok(job) => Some(job),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return Ok(ExitCode::FAILURE);
                    }
                },
            };
            Ok(open_debugger(&mut sess, &p, &cfg, opts, tid, job.as_ref(), &launcher))
        }

        Command::Bench { targets } => {
            let backend = building_backend(opts, "dowel bench", probe)?;
            let requested =
                runnable_targets(&sess, &cfg, targets, dowel_eval::schema::TableKind::Bench)?;
            // 測るものが1つも無い木は、誤りではない。宣言が無いだけである。
            if requested.targets.is_empty() {
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
                eprintln!("no bench targets. declare one with `[bench.<name>]` in dowel.build");
                return Ok(ExitCode::SUCCESS);
            }
            let Some(p) = build(&mut sess, &g, &cfg, opts, &requested.targets, &*backend)? else {
                return Ok(ExitCode::FAILURE);
            };
            // 計測は必ず走らせる。クロスでランナーが無ければここで断る。
            let (launcher, runner_diags) = testing::Launcher::for_config(&sess, &cfg);
            if !runner_diags.is_empty() {
                sess.diagnostics.extend(runner_diags);
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
            }
            // ベンチにハーネスは無いので、数え上げに外部プロセスは走らない。
            let mut jobs = testing::plan_jobs(&sess, &p, &launcher, &requested.targets, &cfg);
            if !requested.cases.is_empty() {
                jobs.retain(|j| requested.cases.contains(&j.label()));
                if jobs.is_empty() {
                    eprintln!("error: nothing matched the requested benchmarks");
                    return Ok(ExitCode::FAILURE);
                }
            }
            let iterations = opts.iterations.unwrap_or(dowel_build::bench::DEFAULT_ITERATIONS);
            let results = dowel_build::bench::measure(&jobs, iterations);
            Ok(report_bench(&results, opts))
        }

        Command::Test { targets } => {
            let backend = building_backend(opts, "dowel test", probe)?;
            let requested = test_targets(&sess, &cfg, targets)?;
            let build_dir = build_plan::build_dir(
                &sess.root_package().map(|p| p.root.clone()).unwrap_or_default(),
                &cfg,
            );
            let mut state = testing::State::load(&build_dir);
            let mut targets = requested.targets.clone();

            // `--debug-failed` は「前回落ちたもの」という選択を含む。
            let only_failed = opts.only_failed || opts.debug_failed;
            if only_failed {
                // 前回落ちたものが1件も無いのは良い知らせであって、意図との
                // 食い違いではない。走らせるものが無いことをそのまま述べる。
                if state.failed().is_empty() {
                    if report(&sess, opts) {
                        return Ok(ExitCode::FAILURE);
                    }
                    let doing = if opts.debug_failed { "debug" } else { "rerun" };
                    eprintln!(
                        "nothing to {doing}. no failing tests were recorded in {}",
                        build_dir.display()
                    );
                    return Ok(ExitCode::SUCCESS);
                }
                // 前回の判定はラベルで持つ。事例のラベルは `<ターゲット>/<事例>`
                // なので、ここではまず組み直す対象を絞り、事例そのものの選別は
                // 起動する組を数え上げた後で行う。
                let failed = state.failed();
                targets.retain(|t| {
                    let label = sess.label(*t);
                    failed.iter().any(|f| *f == label || f.starts_with(&format!("{label}/")))
                });
            }

            // 走らせるものが1つも無い木は、誤りではない。宣言が無いだけである。
            if requested.targets.is_empty() {
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
                eprintln!("no test targets. declare one with `[test.<name>]` in dowel.build");
                return Ok(ExitCode::SUCCESS);
            }
            if targets.is_empty() {
                // ここへ来るのは `--failed` だけ。記録が現実と合わなくなっている。
                return Ok(empty_selection(&sess, opts, &state.failed(), &[]));
            }

            let Some(p) = build(&mut sess, &g, &cfg, opts, &targets, &*backend)? else {
                return Ok(ExitCode::FAILURE);
            };

            // ランナーの解決は起動の直前ではなく、この位置で行って診断を出す。
            // クロス構成でランナーが無いまま起動すると `Exec format error` になり、
            // 構成の誤りがテストの失敗として報告される。
            //
            // `--no-run` では起動しないため、ランナーが無くても構わない。
            // 例外はハーネスで、事例を尋ねること自体が起動である——そちらは
            // 尋ねられなかった失敗として現れる。
            let (launcher, runner_diags) = testing::Launcher::for_config(&sess, &cfg);
            if !opts.no_run && !runner_diags.is_empty() {
                sess.diagnostics.extend(runner_diags);
                if report(&sess, opts) {
                    return Ok(ExitCode::FAILURE);
                }
            }
            // 走らせるものを数え上げてから選別する。事例は起動の単位であり、
            // ターゲットの単位ではない。
            // ハーネスを宣言したターゲットは、実行ファイルに事例を尋ねてから
            // でないと数え上げられない（ADR-0023）。尋ねられなかったものは
            // 0件成功にせず、その場で失敗として持ち回る。
            let (mut jobs, discovery_failures) =
                testing::discover(testing::plan_jobs(&sess, &p, &launcher, &targets, &cfg));
            let known: Vec<String> = jobs.iter().map(|j| j.label()).collect();

            // 選択を順に効かせる。空になったら、そのことを述べて非零で終わる
            // （issue #89 / #91 / #93）——「綴りを間違えた」と「1件通った」が
            // 呼び出し側から同じに見えてはならない。
            if !requested.cases.is_empty() {
                jobs.retain(|j| requested.cases.contains(&j.label()));
            }
            if let Some(wanted) = &opts.labels {
                jobs.retain(|j| j.labels.iter().any(|l| wanted.contains(l)));
            }
            if only_failed {
                let failed = state.failed();
                jobs.retain(|j| failed.contains(&j.label().as_str()));
            }
            // 選択を求めていないのに空になったのは、意図との食い違いではない。
            // 事例が条件で全部落ちた形がこれである（issue #92 / #99）。
            let asked_for_a_selection =
                !requested.cases.is_empty() || opts.labels.is_some() || only_failed;
            if jobs.is_empty() && discovery_failures.is_empty() && asked_for_a_selection {
                let remembered: Vec<String> = if only_failed {
                    state.failed().iter().map(|s| s.to_string()).collect()
                } else {
                    Vec::new()
                };
                let remembered: Vec<&str> = remembered.iter().map(|s| s.as_str()).collect();
                return Ok(empty_selection(&sess, opts, &remembered, &known));
            }

            // 走らせずに並べる。何が走るのかを走らせずに知る手立てが、
            // ここにしか無い（issue #94）。選択が効いた後の一覧である。
            // 「組むだけ」という従来の意味は残る——並べることと矛盾しない。
            if opts.no_run {
                for t in &targets {
                    if let Some(path) = p.artifacts.get(t) {
                        eprintln!("built: {}", path.display());
                    }
                }
                list_cases(&jobs, opts);
                return Ok(ExitCode::SUCCESS);
            }

            // 落ちた事例をデバッガの下で開き直す（docs/30-devexp.md 2.3）。
            // 走らせ直して判定するのではなく、デバッガのセッションが再実行
            // そのものである。判定しないので、記録も更新しない。
            if opts.debug_failed {
                return Ok(debug_failed_case(&mut sess, &p, &cfg, opts, &jobs, &launcher));
            }

            let run_opts = test_run_options(opts);
            let mut outcomes = discovery_failures;
            let requested_count = jobs.len() + outcomes.len();
            outcomes.extend(testing::run(&jobs, &run_opts));

            state.update(&outcomes);
            if let Err(e) = state.save(&p.build_dir) {
                eprintln!("warning: cannot record the test results: {e}");
            }
            Ok(report_tests(&outcomes, requested_count, opts))
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
    backend: &dyn backend::Backend,
) -> Result<Option<build_plan::Plan>, String> {
    let (p, pdiags) = build_plan::plan(sess, g, cfg, requested);
    sess.diagnostics.extend(pdiags);
    if report(sess, opts) {
        return Ok(None);
    }

    write_compdb(sess, &p, opts);

    log_debug!("backend {}", backend.name());
    // ここから先はバックエンドの領分であり、渡すのはビルドグラフだけである
    // （ADR-0018）。
    if let Err(f) = backend::run(backend, &BuildGraph::of(sess, &p), opts.jobs) {
        eprint!("error: {f}");
        return Ok(None);
    }
    Ok(Some(p))
}

/// 編集機向けの `compile_commands.json`。
///
/// どのバックエンドで組むかとは関わりが無い。書き出すだけの `graph` でも書く——
/// 編集機の設定が、選んだバックエンドによって効いたり効かなかったりしてはならない。
fn write_compdb(sess: &Session, p: &build_plan::Plan, opts: &Options) {
    if !opts.compdb {
        return;
    }
    let root = sess.root_package().map(|p| p.root.clone()).unwrap_or_default();
    match compdb::write(p, &root) {
        Ok(paths) => {
            for path in paths {
                log_info!("wrote {}", path.display());
            }
        }
        Err(e) => eprintln!("warning: cannot write compile_commands.json: {e}"),
    }
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
            eprintln!("\n---- {} ----", o.label());
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

/// 計測の報告。数字に合否は無い——失敗と呼ぶのは走らせられなかったことだけ
/// （ADR-0025）。
fn report_bench(results: &[dowel_build::bench::Measurement], opts: &Options) -> ExitCode {
    if opts.message_format == MessageFormat::Json {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for m in results {
            let _ = writeln!(out, "{}", dowel_build::bench::render_json(m));
        }
    }

    let n = results.len();
    eprintln!("measuring {} benchmark{}", n, if n == 1 { "" } else { "s" });
    for m in results {
        eprintln!("{}", m.summary_line());
    }
    let failed: Vec<_> = results.iter().filter(|m| m.failure.is_some()).collect();
    if !failed.is_empty() {
        eprintln!(
            "
failures:"
        );
        for m in &failed {
            eprintln!(
                "
---- {} ----",
                m.label()
            );
            if let Some(why) = &m.failure {
                eprintln!("{why}");
            }
        }
        eprintln!(
            "
bench result: FAILED. {} of {n} could not be measured",
            failed.len()
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// 構成を組み立てる。機能フラグは根のパッケージの `[features]` から解決する。
///
/// `--features` に渡された名前も `[features]` の宣言に照らす。オプション解析の
/// 段では判定できない。値の妥当性は別の語彙（マニフェスト）が決めるものであり、
/// 引数解析にはその情報がない。
fn configure(
    sess: &Session,
    opts: &Options,
    probe: &mut dowel_build::probe::Prober,
) -> Result<(Config, Vec<Diagnostic>), String> {
    let mut diags = Vec::new();
    // 三つ組が様式を決め、様式が道具の既定を決める（ADR-0027）。構成を
    // 三つ組から作るのは、後から `target` を差し替えると既定が付いてこない
    // ためである。
    let mut cfg = match &opts.target {
        Some(t) => Config::for_target(t.clone()),
        None => Config::host_default(),
    };
    cfg.opt = Opt::parse(&opts.config)
        .ok_or_else(|| format!("`--config` must be debug or release (got `{}`)", opts.config))?;

    // ホストの三つ組は、この機械の C コンパイラに訊く（ADR-0028）。dowel が
    // OS と構成から組み立てる綴りは近似であり、道具の名乗りとは別物である
    // （`x86_64-pc-linux-gnu` と `x86_64-unknown-linux-gnu`）。訊かないと、
    // 自分の道具が名乗る綴りを `--target` に渡した利用者がクロス扱いされ、
    // 在るはずのないランナーを求められる。
    //
    // 訊く相手は**ホスト向けの宣言**である。対象向けの `c` に訊くと、返るのは
    // 対象の三つ組でありホストのものではない。
    let host_cc = sess
        .root_package()
        .and_then(|p| p.toolchain.tool("c"))
        .map(|t| t.command.clone())
        .unwrap_or_else(|| dowel_eval::config::default_tool("c", cfg.style).to_string());
    if let Some(named) = probe.triple(&host_cc) {
        log_debug!("host triple: {named} (as named by `{host_cc}`)");
        cfg.set_host(named);
    }
    if let Some(root) = sess.root_package() {
        // 対象の宣言があれば、それ以外のトリプルを求められたときに拒む。
        // ホストには既定の道具があるため、宣言の不在では拒めない——
        // バレメタルの木が `--target` の付け忘れで x86-64 の「ファームウェア
        // 像」として組み上がる（issue #71）。
        if !root.targets.is_empty() && !root.targets.contains(&cfg.target) {
            let mut d = Diagnostic::error(
                "unsupported-target",
                format!("`{}` is not built for `{}`", root.name, cfg.target),
            );
            if let Some(s) = root.targets_site {
                d = d.at(s.file, s.span, "this package declares the targets it supports");
            }
            for t in &root.targets {
                d = d.note(format!("pass --target={t}"));
            }
            diags.push(d);
        }
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
        sess.configure(&mut cfg);

        // ツールチェーンはターゲットトリプルで選ぶ。`[runner.<triple>]` と
        // 同じ形である。宣言の無いトリプルはここで拒む。ホストのコンパイラで
        // 組んで別トリプルの名前を付けると、誤りが qemu の
        // `Invalid ELF image` などとして1段あとに現れる（issue #42）。
        match root.toolchain_for(&cfg.target, cfg.targets_host()) {
            Some(decl) => {
                // 様式が先。道具の既定が様式で変わるので、後に置くと
                // 明示していない道具だけ別の様式の既定に留まる。
                if let Some(style) = decl.style {
                    cfg.set_style(style);
                }
                // 道具の集合は表（dowel_eval::config::TOOLS）が決める。
                for (name, _, _) in dowel_eval::config::TOOLS {
                    if let Some(t) = decl.tool(name) {
                        cfg.set_tool(name, t.command.clone());
                    }
                }
            }
            None => {
                let declared: Vec<&str> = root.toolchains.keys().map(|s| s.as_str()).collect();
                let mut d = Diagnostic::error(
                    "missing-toolchain",
                    format!("no toolchain is declared for target `{}`", cfg.target),
                )
                .note("building with the host toolchain would produce artifacts for the wrong architecture under this target's name");
                // 依存が同じ三つ組の宣言を持っているなら、その値を述べる
                // （issue #125）。持っていることは `toolchain-mismatch` で
                // 別に読み上げていた——探しているものを見つけていながら
                // 「無い」と言い、助言には一般論を出す形になっていた。
                let from_deps = dependency_toolchains(sess, &cfg);
                if from_deps.is_empty() {
                    d = d.note(format!(
                        "declare one, for example `[toolchain.{}]` with `c = \"...\"` in dowel.toml",
                        cfg.target
                    ));
                } else {
                    for line in &from_deps {
                        d = d.note(line.clone());
                    }
                    d = d.note(
                        "a dependency's toolchain does not apply to this build: it is a property \
                         of the build, not of the package (ADR-0031). declare it here to use it",
                    );
                }
                if !declared.is_empty() {
                    d = d.note(format!("toolchains are declared for: {}", declared.join(", ")));
                }
                diags.push(d);
            }
        }
    }
    Ok((cfg, diags))
}

/// 依存パッケージがこの三つ組の道具立てを宣言しているか（issue #125）。
///
/// 採りはしない——道具立ては build 全体の性質であり、依存の性質ではない
/// （[ADR-0031](../../../docs/adr/0031-toolchain-is-the-builds.md)）。
/// それでも、答が手元にあることは述べる。診断が「無い」と言う一方で
/// `toolchain-mismatch` が同じ出力の中で値を読み上げている状態は、
/// 立場を説明していない。
fn dependency_toolchains(sess: &Session, cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for p in sess.packages.iter().skip(1) {
        let Some(decl) = p.toolchain_for(&cfg.target, cfg.targets_host()) else { continue };
        let tools: Vec<String> = dowel_eval::config::TOOLS
            .iter()
            .filter_map(|(name, _, _)| {
                decl.tool(name).map(|t| format!("{name} = \"{}\"", t.command))
            })
            .collect();
        if tools.is_empty() {
            continue;
        }
        out.push(format!(
            "dependency `{}` declares one for this triple ({})",
            p.name,
            tools.join(", ")
        ));
    }
    out
}

/// 名指しの無い既定が及ぶ範囲は、**この木のパッケージ**である（issue #126）。
///
/// 依存パッケージの目標まで数え上げると、使う側の `build` が依存の検査を
/// 組む。ホストの載った三つ組では余計なだけだが、OS の無い三つ組では
/// 落ちる——依存の検査はホスト用に書かれており、使う側のマニフェストには
/// 何の誤りも無い。依存の成果物は、依存として要求されたぶんだけ組まれる。
///
/// 名指しは従来どおり全体から引く。依存の目標を名前で呼べることは変えない
/// ——変えるのは既定であって、到達できる範囲ではない。
fn in_root_package(t: &dowel_model::Target) -> bool {
    // 根は読み込みの先頭である（`Session::root_package`）。
    t.package == dowel_model::PackageId(0)
}

/// 対象の決定。指定がなければ、この木の全ての bin と test。
fn default_targets(
    sess: &Session,
    cfg: &Config,
    requested: &[String],
) -> Result<Vec<dowel_model::TargetId>, String> {
    if !requested.is_empty() {
        return requested.iter().map(|s| sess.find_target(s)).collect();
    }
    use dowel_eval::schema::TableKind;
    // 三つ組の外にある目標は、名指しの無い数え上げからは**黙って外れる**
    // （issue #126）。名指しされたときは `unsupported-target` で断る——
    // 既定は「この構成で作れるもの」であり、名指しは要求だからである。
    let buildable = |t: &&dowel_model::Target| {
        in_root_package(t) && build_plan::supports_target(sess, t.id, cfg)
    };
    let out: Vec<_> = sess
        .targets
        .iter()
        .filter(buildable)
        .filter(|t| matches!(t.kind, TableKind::Bin | TableKind::Test | TableKind::Bench))
        .map(|t| t.id)
        .collect();
    if out.is_empty() {
        // ライブラリしかない場合はそれを作る。ここもこの木の中だけ。
        return Ok(sess.targets.iter().filter(buildable).map(|t| t.id).collect());
    }
    Ok(out)
}

/// `inspect` の対象。指定がなければ、検査を宣言している全ターゲット。
fn inspect_targets(
    sess: &Session,
    requested: &[String],
) -> Result<Vec<dowel_model::TargetId>, String> {
    if !requested.is_empty() {
        // 明示された対象は、検査を持たなくても断らない。持たないことは
        // 誤りではなく、報告するものが無いだけである。
        return requested.iter().map(|s| sess.find_target(s)).collect();
    }
    Ok(sess.targets.iter().filter(|t| !t.inspections.is_empty()).map(|t| t.id).collect())
}

/// 宣言された検査を走らせ、道具の出力を見せる。真を返せば失敗。
///
/// 出力はそのまま通す。dowel は解釈しない——`size` の書式は実装ごとに
/// 違い、読み解くのは道具の側の仕事である（issue #60）。判定に使う形
/// （予算の宣言）は、その解釈が要るため別の決定になる。
fn inspect(
    sess: &Session,
    cfg: &Config,
    plan: &build_plan::Plan,
    requested: &[dowel_model::TargetId],
    opts: &Options,
) -> bool {
    let mut failed = false;
    for &tid in requested {
        let target = sess.target(tid);
        let Some(artifact) = plan.artifacts.get(&tid) else { continue };
        for decl in &target.inspections {
            let mut args: Vec<String> = decl
                .args
                .as_ref()
                .and_then(|v| dowel_eval::specialize(v, cfg))
                .map(|v| dowel_build::flatten_strs(&v))
                .unwrap_or_default();
            // 成果物は末尾に位置で置く（ADR-0008）。
            args.push(artifact.display().to_string());
            let program = cfg.tool(&decl.tool);

            let out = std::process::Command::new(program).args(&args).output();
            let label = sess.label(tid);
            match out {
                Ok(out) => {
                    let ok = out.status.success();
                    failed |= !ok;
                    let text = String::from_utf8_lossy(&out.stdout).to_string();
                    let errors = String::from_utf8_lossy(&out.stderr).to_string();
                    match opts.message_format {
                        MessageFormat::Json => {
                            let mut w = JsonWriter::new();
                            w.begin_object();
                            w.field_str("target", &label);
                            w.field_str("inspection", &decl.suffix);
                            w.field_str("tool", &decl.tool);
                            w.key("command").begin_array();
                            w.str(program);
                            for a in &args {
                                w.str(a);
                            }
                            w.end_array();
                            w.field_bool("ok", ok);
                            w.field_str("output", &text);
                            w.end_object();
                            println!("{}", w.finish());
                        }
                        MessageFormat::Human => {
                            eprintln!("== {label}: {} ({}) ==", decl.suffix, decl.tool);
                            print!("{text}");
                            if !ok {
                                eprint!("{errors}");
                                eprintln!(
                                    "`{}` exited with {}",
                                    decl.tool,
                                    status_text(&out.status)
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    // 実在は計画段で確かめていない——検査は計画に載らない。
                    // ここで断り、宣言と実体の食い違いとして述べる。
                    failed = true;
                    eprintln!(
                        "error: cannot run `{program}` for {label}: {e}\n  \
                         note: it comes from `[toolchain] {}`; it must be on PATH",
                        decl.tool
                    );
                }
            }
        }
    }
    failed
}

fn status_text(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(c) => format!("exit code {c}"),
        None => "a signal".to_string(),
    }
}

/// `test` の対象。指定がなければ全ての test ターゲット。
/// 選択が空になった。何を選ぼうとして空になったのかを述べ、非零で終わる。
///
/// 「ラベルの綴りを間違えた」と「1件通った」が、呼び出し側から同じに見えては
/// ならない（issue #89）。報告は stderr に出るため、状態を 0 にすると CI の
/// ログでは埋もれる。`--failed` も同じ問いであり、同じ答にする（issue #91）。
///
/// `remembered` は `--failed` が覚えているラベル、`known` は今回計画された
/// 事例のラベル。両方あるとき、消えた事例を名指せる。
/// 落ちた事例をデバッガの下で開き直す（`dowel test --debug-failed`）。
///
/// デバッガは対話するものであり、繋がる相手は1つである。落ちたものが
/// 複数残っているなら、選ばずに並べて、名指しを求める——こちらが選ぶと、
/// 「どれが開いたのか」を利用者が推測することになる。
fn debug_failed_case(
    sess: &mut Session,
    p: &build_plan::Plan,
    cfg: &Config,
    opts: &Options,
    jobs: &[testing::Job],
    launcher: &testing::Launcher,
) -> ExitCode {
    let job = match jobs {
        [one] => one,
        many => {
            eprintln!("error: {} tests failed last time; the debugger attaches to one", many.len());
            for j in many {
                eprintln!("  {}", j.label());
            }
            eprintln!("note: name one: `dowel test <label> --debug-failed`");
            return ExitCode::FAILURE;
        }
    };
    open_debugger(sess, p, cfg, opts, job.target, Some(job), launcher)
}

/// 名指しされた事例の仕事を1つ取り出す（issue #110）。
///
/// 数え上げは `dowel test` と同じ経路を通る。ハーネスを宣言した目標では
/// 実行ファイルに尋ねることになるので、外部プロセスが1つ走る。
fn find_case(
    sess: &Session,
    p: &build_plan::Plan,
    cfg: &Config,
    launcher: &testing::Launcher,
    tid: dowel_model::TargetId,
    name: &str,
) -> Result<testing::Job, String> {
    use dowel_eval::schema::TableKind;
    let kind = sess.target(tid).kind;
    if !matches!(kind, TableKind::Test | TableKind::Bench) {
        return Err(format!(
            "`{}` is a {} target; only `test` and `bench` targets have cases",
            sess.label(tid),
            kind.name()
        ));
    }
    let wanted = format!("{}/{name}", sess.label(tid));
    let (jobs, failures) = testing::discover(testing::plan_jobs(sess, p, launcher, &[tid], cfg));
    // 列挙できなかった目標は、事例が無いのではなく尋ねられなかったのである。
    if let Some(f) = failures.first() {
        return Err(f.failure_reason().unwrap_or_else(|| "the cases could not be listed".into()));
    }
    if let Some(job) = jobs.iter().find(|j| j.label() == wanted) {
        return Ok(job.clone());
    }
    let known: Vec<String> = jobs.iter().map(|j| j.label()).collect();
    let mut msg = format!("no case named `{wanted}`");
    if !known.is_empty() {
        msg.push_str(&format!("\nnote: this target has: {}", known.join(", ")));
    }
    Err(msg)
}

/// 構成を組み立ててデバッガを開く。`dowel debug` と `--debug-failed` の
/// 共通の出口である——2つの入口が別々の構成を作ると、同じラベルを指しても
/// 開くものが違いうる。
fn open_debugger(
    sess: &mut Session,
    p: &build_plan::Plan,
    cfg: &Config,
    opts: &Options,
    tid: dowel_model::TargetId,
    job: Option<&testing::Job>,
    launcher: &testing::Launcher,
) -> ExitCode {
    let mut launch = match dowel_build::debug::prepare(sess, p, cfg, tid) {
        Ok(s) => s,
        Err(d) => {
            sess.diagnostics.push(d);
            report(sess, opts);
            return ExitCode::FAILURE;
        }
    };
    if let Some(job) = job {
        // 事例の宣言を引き継ぐ。`job.args` の先頭はランナー由来の分なので、
        // 事例そのものの引数だけを残す——デバッグの経路ではランナーは
        // スタブとして別に組まれる。
        let runner_args = match &job.binary {
            Some(b) => launcher.command(b).1.len(),
            None => 0,
        };
        launch.args = job.args.iter().skip(runner_args).cloned().collect();
        launch.env = job.env.clone();
        launch.cwd = job.cwd.clone();
        eprintln!("debugging {}", job.label());
    }

    if opts.dap {
        // 構成は成果物なので stdout。進行は stderr のまま。
        println!("{}", dowel_build::debug::dap(&launch));
        return ExitCode::SUCCESS;
    }
    // デバッガは対話するものである。実在しなければ、起動して
    // 「見つからない」と言われるより先に述べる。
    if !dowel_build::exec::program_exists(&launch.debugger) {
        sess.diagnostics.push(
            Diagnostic::error(
                "missing-toolchain",
                format!("the debugger `{}` is not on PATH", launch.debugger),
            )
            .note("declare it with `debug = \"...\"` in `[toolchain]` or `[toolchain.<triple>]`")
            .note("`--dap` writes the launch configuration without starting anything"),
        );
        report(sess, opts);
        return ExitCode::FAILURE;
    }
    match dowel_build::debug::run(&launch) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn empty_selection(
    sess: &Session,
    opts: &Options,
    remembered: &[&str],
    known: &[String],
) -> ExitCode {
    if report(sess, opts) {
        return ExitCode::FAILURE;
    }
    if let Some(wanted) = &opts.labels {
        eprintln!(
            "error: no test carries {}. labels are declared in `[test.<name>.cases]`",
            wanted.iter().map(|l| format!("`{l}`")).collect::<Vec<_>>().join(" or ")
        );
        if !known.is_empty() {
            eprintln!("note: `dowel test --no-run` lists the cases and their labels");
        }
        return ExitCode::FAILURE;
    }
    if opts.only_failed || opts.debug_failed {
        // ここへ来るのは「覚えているものはあるが、どれも今は存在しない」場合
        // だけである（何も落ちていない場合は先に返している）。記録が現実と
        // 合わなくなっている。消えた事例を名指す——「直す」という行為そのものが、
        // 記録と現実を食い違わせる契機になる。
        let gone: Vec<&&str> =
            remembered.iter().filter(|r| !known.iter().any(|k| k == **r)).collect();
        for label in &gone {
            eprintln!("warning: `{label}` failed last time but no longer exists");
        }
        eprintln!(
            "error: no remembered failure is still present. run `dowel test` to start a new record"
        );
        return ExitCode::FAILURE;
    }
    eprintln!("error: nothing matched the requested tests");
    if !known.is_empty() {
        eprintln!("note: `dowel test --no-run` lists the cases that exist");
    }
    ExitCode::FAILURE
}

/// 走るはずのものを並べる（`--no-run`、issue #94）。
///
/// 事例は選択の単位になったのに、一覧の単位になっていなかった。ラベルの語彙を
/// 確かめる先も、重い事例を見分ける先も、ここ以外に無い。
fn list_cases(jobs: &[testing::Job], opts: &Options) {
    if opts.message_format == MessageFormat::Json {
        // 走らせたときと同じ欄で出す。結果と突き合わせるのは下流の仕事であり、
        // 綴りが揃っていなければ突き合わせられない（issue #100）。
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for j in jobs {
            let mut w = JsonWriter::new();
            w.begin_object();
            w.field_str("kind", "test-case");
            w.field_str("target", &j.target_label);
            match &j.case {
                Some(c) => w.field_str("case", c),
                None => w.key("case").null(),
            };
            w.field_str("label", &j.label());
            w.field_strs("labels", j.labels.iter().map(|l| l.as_str()));
            w.field_bool("should_fail", j.should_fail);
            match j.timeout {
                Some(t) => w.key("timeout").u64(t.as_secs()),
                None => w.key("timeout").null(),
            };
            w.end_object();
            let _ = writeln!(out, "{}", w.finish());
        }
        return;
    }
    let width = jobs.iter().map(|j| j.label().len()).max().unwrap_or(0);
    for j in jobs {
        let mut notes = Vec::new();
        if !j.labels.is_empty() {
            notes.push(format!("[{}]", j.labels.join(", ")));
        }
        if j.should_fail {
            notes.push("should_fail".to_string());
        }
        if let Some(t) = j.timeout {
            notes.push(format!("timeout {}s", t.as_secs()));
        }
        if notes.is_empty() {
            eprintln!("{}", j.label());
        } else {
            eprintln!("{:width$}  {}", j.label(), notes.join(" "));
        }
    }
}

/// `dowel test` の位置引数。
///
/// 目標の参照（`app:unit`）でも、事例のラベル（`app:unit/parse`）でもよい
/// （issue #93）。画面に出た識別子をそのまま貼り戻せることは、道具として
/// 基本的な性質である——落ちた1件だけを再実行する経路がここにしかない。
struct Requested {
    /// 組む対象。事例を指した場合はその事例の属する目標
    targets: Vec<dowel_model::TargetId>,
    /// 指定された事例ラベルの完全形。空なら目標の全事例
    cases: std::collections::BTreeSet<String>,
}

fn test_targets(sess: &Session, cfg: &Config, requested: &[String]) -> Result<Requested, String> {
    runnable_targets(sess, cfg, requested, dowel_eval::schema::TableKind::Test)
}

/// `test` と `bench` に共通の、目標と事例の数え上げ。
fn runnable_targets(
    sess: &Session,
    cfg: &Config,
    requested: &[String],
    kind: dowel_eval::schema::TableKind,
) -> Result<Requested, String> {
    if requested.is_empty() {
        // 既定はこの木の中で、この三つ組へ組めるものだけ（issue #126）。
        // 依存の検査は依存の作者が走らせる。
        return Ok(Requested {
            targets: sess
                .targets
                .iter()
                .filter(|t| {
                    t.kind == kind
                        && in_root_package(t)
                        && build_plan::supports_target(sess, t.id, cfg)
                })
                .map(|t| t.id)
                .collect(),
            cases: std::collections::BTreeSet::new(),
        });
    }
    let mut targets = Vec::new();
    let mut cases = std::collections::BTreeSet::new();
    for arg in requested {
        // 事例の名前に `/` は書けない（検証済み）ので、最初の `/` が境目になる。
        let (target_ref, case) = match arg.split_once('/') {
            Some((t, c)) => (t, Some(c)),
            None => (arg.as_str(), None),
        };
        let id = sess.find_target(target_ref)?;
        let t = sess.target(id);
        // 明示指定が種別違いなら、黙って走らせずに断る。
        if t.kind != kind {
            return Err(format!(
                "`{}` is a {} target, not a {}",
                sess.label(id),
                t.kind.name(),
                kind.name()
            ));
        }
        if let Some(case) = case {
            cases.insert(format!("{}/{case}", sess.label(id)));
        }
        if !targets.contains(&id) {
            targets.push(id);
        }
    }
    Ok(Requested { targets, cases })
}

/// 成果物を要求するコマンドのためのバックエンド。
///
/// 書き出すだけのバックエンド（`graph`）では走らせるものが無い。
/// 黙って何もしないより断る（ADR-0018）。
fn building_backend(
    opts: &Options,
    command: &str,
    probe: &mut dowel_build::probe::Prober,
) -> Result<Box<dyn backend::Backend>, String> {
    let backend = backend::select(opts.backend.as_deref(), probe)?;
    if !backend.builds() {
        return Err(format!(
            "`{}` writes the build description but does not build. \
             `{command}` needs a backend that does",
            backend.name()
        ));
    }
    Ok(backend)
}

/// ビルドを行わないバックエンド（`graph`）で組み立てだけを行う。
///
/// 成果物が出来ていないのに「built:」と述べると、そこに無いものを指すことに
/// なる。書き出したファイルを述べる。
fn emit_only(
    sess: &mut Session,
    g: &dowel_model::Graph,
    cfg: &Config,
    opts: &Options,
    requested: &[dowel_model::TargetId],
    backend: &dyn backend::Backend,
) -> Result<ExitCode, String> {
    let (p, pdiags) = build_plan::plan(sess, g, cfg, requested);
    sess.diagnostics.extend(pdiags);
    if report(sess, opts) {
        return Ok(ExitCode::FAILURE);
    }
    write_compdb(sess, &p, opts);
    match backend.emit(&BuildGraph::of(sess, &p)) {
        Ok(paths) => {
            for path in paths {
                eprintln!("wrote: {}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(f) => {
            eprint!("error: {f}");
            Ok(ExitCode::FAILURE)
        }
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
    // 道具について確かめたことは、プロジェクトの外に置く（ADR-0028）。
    // 同じ表示に並べるのは、消えたときに探す先が2つあることを知らせるため。
    let facts = dowel_store::Facts::open();
    println!("facts      {}", dowel_store::facts::Facts::dir().display());
    println!("  format   {}", dowel_store::facts::FORMAT);
    println!("  records  {}", facts.len());
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

    // 入れ子の表はプロパティのブロックではないため、`blocks` とは別に出す
    // （issue #60）。一覧は `schema::NESTED_TABLES` が持つ——ここで数え上げると、
    // 型検査器だけが知っている表ができる（issue #90）。
    for t in schema::NESTED_TABLES {
        w.key(t.dump_key).begin_array();
        for p in (t.props)() {
            w.begin_object();
            w.field_str("name", p.name);
            w.field_str("type", &p.ty.display());
            w.field_str("merge", p.merge.name());
            w.field_str("doc", p.doc);
            w.end_object();
        }
        w.end_array();
    }

    // ランナーは表種別であってターゲットではない。プロパティの集合も
    // ターゲットのものとは別である。
    w.key("runner_properties").begin_array();
    for p in schema::runner_props() {
        w.begin_object();
        w.field_str("name", p.name);
        w.field_str("type", &p.ty.display());
        w.field_str("merge", p.merge.name());
        w.field_str("doc", p.doc);
        w.end_object();
    }
    w.end_array();

    // 既定は様式で変わる（ADR-0027）。1つだけ出すと、もう片方の様式では
    // 嘘になる。
    w.key("tools").begin_array();
    for (name, gnu, msvc) in dowel_eval::config::TOOLS {
        w.begin_object();
        w.field_str("name", name);
        w.field_str("default_gnu", gnu);
        w.field_str("default_msvc", msvc);
        w.end_object();
    }
    w.end_array();

    w.key("toolchain_styles").begin_array();
    for name in dowel_eval::config::Style::ALL {
        w.str(name);
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

    // パッケージの定数は構成ではない（ADR-0020）。値域も網羅性も持たず、
    // `match` の被検査対象にもならない。同じ表に混ぜると、版でビルドを
    // 分岐できると述べることになる。
    w.key("pkg_constants").begin_array();
    for (name, doc) in dowel_eval::config::PKG_CONSTANTS {
        w.begin_object();
        w.field_str("name", &format!("pkg.{name}"));
        w.field_str("type", "Str");
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
