//! テストの実行。
//!
//! `test` 種別のターゲットはビルドすると実行ファイルになる。本モジュールは
//! それを起動し、終了状態を収集する。テストハーネスは提供しない。
//! 「終了状態 0 なら成功」という C の慣習に従い、枠組みは利用者側に委ねる。
//!
//! 起動の直前に [`Launcher`] を経由する。ここがランナー抽象
//! （qemu / SSH / 実機、docs/30-devexp.md 1節）の接続点である。
//! クロス実行では成果物を直接起動できないため、この箇所のみが変わる。
//!
//! 前回の結果は [`State`] としてビルドディレクトリに保存し、`--failed` が読む。
//! 形式を JSON にしないのは、読み出し側の実装が必要になるためである。
//! 利用者向けの出力ではなく内部状態であり、行指向で足りる。

use crate::plan::Plan;
use dowel_model::{Session, TargetId};
use dowel_support::{log_debug, log_trace};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// 成果物を起動するコマンドを組み立てる（docs/30-devexp.md 1節）。
///
/// ターゲットトリプルごとに宣言された `[runner.<triple>]` を引き、
/// 「何で包んで起動するか」を決める。宣言が無ければそのまま起動する。
///
/// ## ホストと異なるトリプルでランナーが宣言されていない場合
///
/// そのまま起動すると `Exec format error` になり、テストの失敗として報告される。
/// 原因は構成にあってテスト対象のコードにはないため、起動前に構成の診断として出す。
pub struct Launcher {
    /// ラッパのプログラム。ホスト実行なら `None`
    program: Option<String>,
    args: Vec<String>,
}

impl Launcher {
    /// 構成に対応するランナーを引く。
    ///
    /// 診断を返すのは「クロスなのにランナーが無い」場合のみ。
    pub fn for_config(
        sess: &Session,
        cfg: &dowel_eval::Config,
    ) -> (Launcher, Vec<dowel_support::Diagnostic>) {
        let mut diags = Vec::new();
        let Some(runner) = sess.runners.get(&cfg.target) else {
            if cfg.target != dowel_eval::config::default_triple() {
                let declared: Vec<&str> = sess.runners.keys().map(|s| s.as_str()).collect();
                let mut d = dowel_support::Diagnostic::error(
                    "missing-runner",
                    format!("no runner is declared for `{}`", cfg.target),
                )
                .note("the artifact is built for another machine and cannot be started here")
                .note("declare one, for example `[runner.<triple>]` with `command = \"qemu-...\"`");
                if !declared.is_empty() {
                    d = d.note(format!("runners are declared for: {}", declared.join(", ")));
                }
                diags.push(d);
            }
            log_debug!("no runner for `{}`; starting artifacts directly", cfg.target);
            return (Launcher { program: None, args: Vec::new() }, diags);
        };

        // ランナーの値も `match` や後置 `when` を持ちうる。具体化はここで行う。
        let program = runner
            .prop("command")
            .and_then(|v| dowel_eval::specialize(v, cfg))
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        let args = runner
            .prop("args")
            .and_then(|v| dowel_eval::specialize(v, cfg))
            .map(|v| string_list(&v))
            .unwrap_or_default();

        match program {
            Some(program) => {
                log_debug!("runner for `{}`: {program} {}", cfg.target, args.join(" "));
                (Launcher { program: Some(program), args }, diags)
            }
            None => {
                // `command` の存在と型は読み込み時に検証済み。ここへ来るのは
                // 構成によって値が消えた場合（`when` が全て偽など）である。
                diags.push(
                    dowel_support::Diagnostic::error(
                        "missing-runner",
                        format!("runner `{}` has no `command` in this configuration", cfg.target),
                    )
                    .at(runner.site.file, runner.site.span, "declared here")
                    .note("a `when` clause may have removed it"),
                );
                (Launcher { program: None, args: Vec::new() }, diags)
            }
        }
    }

    /// ラッパを持たない起動器。ランナーを要さない経路と試験のために使う。
    pub fn direct() -> Launcher {
        Launcher { program: None, args: Vec::new() }
    }

    /// `binary` を起動するためのプログラムと引数。
    pub fn command(&self, binary: &Path) -> (String, Vec<String>) {
        match &self.program {
            None => (binary.display().to_string(), Vec::new()),
            Some(program) => {
                let mut args = self.args.clone();
                args.push(binary.display().to_string());
                (program.clone(), args)
            }
        }
    }
}

/// 具体化済みの `List<Str>` を取り出す。
fn string_list(v: &dowel_eval::Value) -> Vec<String> {
    match &v.data {
        dowel_eval::Data::List(items) => {
            items.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect()
        }
        _ => Vec::new(),
    }
}

/// 実行のしかた。
#[derive(Clone, Copy, Debug)]
pub struct RunOptions {
    /// 子プロセスの出力を捕まえる。偽なら素通しする
    pub capture: bool,
    /// 最初の失敗で打ち切る
    pub fail_fast: bool,
    /// 同時に走らせる本数
    pub jobs: usize,
}

impl Default for RunOptions {
    fn default() -> RunOptions {
        // 既定を逐次にするのは、C のテストが共有資源（同じ作業ディレクトリ、
        // 固定のポート、書き出し先のファイル）を用いる場合があるためである。
        // 並列を既定にすると、順序に依存する失敗が再現しない形で発生する。
        // 並列実行は明示的に指定させる。
        RunOptions { capture: true, fail_fast: false, jobs: 1 }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub target: TargetId,
    pub label: String,
    pub binary: PathBuf,
    /// プロセスを起動できなかった場合は `None`
    pub status: Option<i32>,
    pub passed: bool,
    pub duration_ms: u128,
    /// `capture` が真のときのみ中身を持つ
    pub stdout: String,
    pub stderr: String,
    /// 起動そのものに失敗した理由
    pub launch_error: Option<String>,
}

impl Outcome {
    /// 1行の結果表示。`test <ラベル> ... ok (12ms)`
    pub fn summary_line(&self) -> String {
        let verdict = if self.passed { "ok" } else { "FAILED" };
        format!("test {} ... {verdict} ({}ms)", self.label, self.duration_ms)
    }

    /// 失敗の理由を1行で。成功時は `None`。
    pub fn failure_reason(&self) -> Option<String> {
        if self.passed {
            return None;
        }
        Some(match (&self.launch_error, self.status) {
            (Some(e), _) => format!("could not start the test binary: {e}"),
            (None, Some(code)) => format!("exited with status {code}"),
            (None, None) => "terminated by a signal".to_string(),
        })
    }
}

/// 1本のテストを起動するために必要な情報。
///
/// `Session` から分離しているのは、並列実行の作業スレッドがモデルを参照しない
/// ようにするためである。`Session` は増分エンジンのメモ表を保持しており、
/// スレッド間で共有できない。起動対象の決定は逐次に行い、スレッドは起動のみを担う。
#[derive(Clone, Debug)]
struct Job {
    target: TargetId,
    label: String,
    /// 計画に成果物が無い場合は `None`
    binary: Option<PathBuf>,
    cwd: PathBuf,
    program: String,
    args: Vec<String>,
}

fn plan_jobs(sess: &Session, plan: &Plan, launcher: &Launcher, targets: &[TargetId]) -> Vec<Job> {
    targets
        .iter()
        .map(|&tid| {
            let binary = plan.artifacts.get(&tid).cloned();
            let (program, args) = match &binary {
                Some(b) => launcher.command(b),
                None => (String::new(), Vec::new()),
            };
            Job {
                target: tid,
                label: sess.label(tid),
                binary,
                // 作業ディレクトリはパッケージルート。テストが読む固定資産の相対パスが、
                // マニフェストに書いたものと同じ基準で解決されるようにする。
                cwd: sess.package(sess.target(tid).package).root.clone(),
                program,
                args,
            }
        })
        .collect()
}

/// 与えられたテストターゲットを起動する。
///
/// 戻り値は要求順。`fail_fast` で打ち切った場合、起動しなかったものは含まれない。
/// 呼び出し側は要求数との差から未実行の件数を得る。
pub fn run(
    sess: &Session,
    plan: &Plan,
    launcher: &Launcher,
    targets: &[TargetId],
    opts: &RunOptions,
) -> Vec<Outcome> {
    let _phase = dowel_support::log::Phase::start("test");
    let jobs = opts.jobs.max(1).min(targets.len().max(1));
    log_debug!("running {} tests with {jobs} job(s)", targets.len());
    let planned = plan_jobs(sess, plan, launcher, targets);
    for j in &planned {
        log_trace!(
            "  planned {}: {} (cwd {})",
            j.label,
            if j.program.is_empty() { "<no artifact>" } else { &j.program },
            j.cwd.display()
        );
    }

    if jobs == 1 {
        let mut out = Vec::new();
        for job in &planned {
            let outcome = run_one(job, opts.capture);
            let failed = !outcome.passed;
            out.push(outcome);
            if failed && opts.fail_fast {
                log_debug!("stopping early: fail-fast");
                break;
            }
        }
        return out;
    }

    // 並列。要求順を保つため添字ごと集めて最後に並べ替える。
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let collected: Mutex<Vec<(usize, Outcome)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                if opts.fail_fast && stop.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = planned.get(i) else { break };
                let outcome = run_one(job, opts.capture);
                if !outcome.passed {
                    stop.store(true, Ordering::Relaxed);
                }
                collected.lock().expect("the results mutex is poisoned").push((i, outcome));
            });
        }
    });
    let mut collected = collected.into_inner().expect("the results mutex is poisoned");
    collected.sort_by_key(|(i, _)| *i);
    collected.into_iter().map(|(_, o)| o).collect()
}

fn run_one(job: &Job, capture: bool) -> Outcome {
    let Job { target: tid, label, binary, cwd, program, args } = job;
    let (label, cwd) = (label.clone(), cwd.clone());
    let Some(binary) = binary.clone() else {
        return Outcome {
            target: *tid,
            label,
            binary: PathBuf::new(),
            status: None,
            passed: false,
            duration_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            launch_error: Some("no artifact was planned for this target".into()),
        };
    };
    let tid = *tid;

    log_debug!("running {label}");
    log_trace!("  {program} (cwd {})", cwd.display());

    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(&cwd);
    let start = Instant::now();
    let result = if capture {
        cmd.output().map(|o| {
            (
                o.status,
                String::from_utf8_lossy(&o.stdout).to_string(),
                String::from_utf8_lossy(&o.stderr).to_string(),
            )
        })
    } else {
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        cmd.status().map(|s| (s, String::new(), String::new()))
    };
    let duration_ms = start.elapsed().as_millis();

    match result {
        Ok((status, stdout, stderr)) => Outcome {
            target: tid,
            label,
            binary,
            status: status.code(),
            passed: status.success(),
            duration_ms,
            stdout,
            stderr,
            launch_error: None,
        },
        Err(e) => Outcome {
            target: tid,
            label,
            binary,
            status: None,
            passed: false,
            duration_ms,
            stdout: String::new(),
            stderr: String::new(),
            launch_error: Some(e.to_string()),
        },
    }
}

/// 前回の結果。`--failed` が読む。
///
/// ビルドディレクトリに置き、構成ごとに分ける。
/// 形式は行指向とする。JSON にすると読み出し側の実装が必要になるが、
/// これは利用者向けの出力ではなく内部状態であり、その必要はない。
pub struct State {
    /// ターゲットのラベル → 前回通ったか
    pub results: std::collections::BTreeMap<String, bool>,
}

const STATE_FILE: &str = "test-state.tsv";

impl State {
    pub fn load(build_dir: &Path) -> State {
        let mut results = std::collections::BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(build_dir.join(STATE_FILE)) {
            for line in text.lines() {
                if line.starts_with('#') || line.trim().is_empty() {
                    continue;
                }
                if let Some((verdict, label)) = line.split_once('\t') {
                    results.insert(label.to_string(), verdict == "ok");
                }
            }
        }
        log_debug!("loaded {} previous test results", results.len());
        State { results }
    }

    /// 今回走らせた分で上書きする。走らせなかったものは前回の判定を残す。
    pub fn update(&mut self, outcomes: &[Outcome]) {
        for o in outcomes {
            self.results.insert(o.label.clone(), o.passed);
        }
    }

    pub fn failed(&self) -> Vec<&str> {
        self.results.iter().filter(|(_, ok)| !**ok).map(|(l, _)| l.as_str()).collect()
    }

    pub fn save(&self, build_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(build_dir)?;
        let mut text = String::from("# dowel test state. <ok|failed>\\t<target>\n");
        for (label, ok) in &self.results {
            text.push_str(if *ok { "ok\t" } else { "failed\t" });
            text.push_str(label);
            text.push('\n');
        }
        std::fs::write(build_dir.join(STATE_FILE), text)
    }
}

/// 機械可読な結果。1件1行の JSON とし、逐次消費できるようにする。
pub fn render_json(o: &Outcome) -> String {
    let mut w = dowel_support::json::JsonWriter::new();
    w.begin_object();
    w.field_str("kind", "test-result");
    w.field_str("target", &o.label);
    w.field_str("binary", &o.binary.display().to_string());
    w.field_bool("passed", o.passed);
    match o.status {
        Some(c) => w.key("exit_status").i64(c as i64),
        None => w.key("exit_status").null(),
    };
    w.field_u64("duration_ms", o.duration_ms as u64);
    w.field_str("stdout", &o.stdout);
    w.field_str("stderr", &o.stderr);
    match &o.launch_error {
        Some(e) => w.field_str("launch_error", e),
        None => w.key("launch_error").null(),
    };
    w.end_object();
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(passed: bool, status: Option<i32>, launch_error: Option<&str>) -> Outcome {
        Outcome {
            target: TargetId(0),
            label: "pkg:unit".into(),
            binary: PathBuf::from("/tmp/unit"),
            status,
            passed,
            duration_ms: 12,
            stdout: String::new(),
            stderr: String::new(),
            launch_error: launch_error.map(|s| s.to_string()),
        }
    }

    #[test]
    fn summary_line_shows_the_verdict_and_duration() {
        assert_eq!(outcome(true, Some(0), None).summary_line(), "test pkg:unit ... ok (12ms)");
        assert_eq!(outcome(false, Some(1), None).summary_line(), "test pkg:unit ... FAILED (12ms)");
    }

    #[test]
    fn failure_reason_distinguishes_the_three_cases() {
        assert_eq!(outcome(true, Some(0), None).failure_reason(), None);
        assert_eq!(outcome(false, Some(3), None).failure_reason().unwrap(), "exited with status 3");
        // 状態コードが無いのはシグナルで落ちた場合。
        assert_eq!(outcome(false, None, None).failure_reason().unwrap(), "terminated by a signal");
        assert!(outcome(false, None, Some("no such file"))
            .failure_reason()
            .unwrap()
            .contains("could not start"));
    }

    #[test]
    fn json_carries_the_verdict_and_output() {
        let mut o = outcome(false, Some(2), None);
        o.stdout = "hello\n".into();
        let json = render_json(&o);
        assert!(json.contains(r#""kind":"test-result""#), "{json}");
        assert!(json.contains(r#""passed":false"#), "{json}");
        assert!(json.contains(r#""exit_status":2"#), "{json}");
        assert!(json.contains(r#""stdout":"hello\n""#), "{json}");
        assert!(json.contains(r#""launch_error":null"#), "{json}");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn state_round_trips_through_the_build_directory() {
        let dir = scratch("test-state");
        let mut st = State { results: Default::default() };
        st.update(&[outcome(true, Some(0), None)]);
        st.save(&dir).unwrap();

        let loaded = State::load(&dir);
        assert_eq!(loaded.results.get("pkg:unit"), Some(&true));
        assert!(loaded.failed().is_empty());
    }

    #[test]
    fn state_keeps_targets_that_were_not_rerun() {
        let dir = scratch("test-state-merge");
        let mut st = State { results: Default::default() };
        st.results.insert("pkg:a".into(), false);
        st.results.insert("pkg:b".into(), true);
        st.save(&dir).unwrap();

        // `pkg:a` だけ走らせ直して通った場合、`pkg:b` の判定は残る。
        let mut st = State::load(&dir);
        let mut rerun = outcome(true, Some(0), None);
        rerun.label = "pkg:a".into();
        st.update(&[rerun]);
        assert_eq!(st.results.get("pkg:a"), Some(&true));
        assert_eq!(st.results.get("pkg:b"), Some(&true));
        assert!(st.failed().is_empty());
    }

    #[test]
    fn failed_lists_only_the_failures() {
        let mut st = State { results: Default::default() };
        st.results.insert("pkg:a".into(), false);
        st.results.insert("pkg:b".into(), true);
        st.results.insert("pkg:c".into(), false);
        assert_eq!(st.failed(), vec!["pkg:a", "pkg:c"]);
    }

    #[test]
    fn a_missing_state_file_reads_as_empty() {
        let dir = scratch("test-state-missing");
        assert!(State::load(&dir).results.is_empty());
    }

    #[test]
    fn the_default_run_is_sequential_and_captures() {
        // 既定を逐次にする理由は RunOptions のコメントにある。
        let o = RunOptions::default();
        assert_eq!(o.jobs, 1);
        assert!(o.capture);
        assert!(!o.fail_fast);
    }

    #[test]
    fn without_a_runner_the_artifact_is_started_directly() {
        let (program, args) = Launcher::direct().command(Path::new("/tmp/unit"));
        assert_eq!(program, "/tmp/unit");
        assert!(args.is_empty());
    }

    #[test]
    fn a_runner_wraps_the_artifact_and_keeps_its_arguments_in_front() {
        // 成果物のパスは引数の**末尾**に来る。`qemu-riscv64 -L <sysroot> <binary>`
        // のように、ラッパの引数が先で成果物が後という並びが求められる。
        let l = Launcher {
            program: Some("qemu-riscv64".into()),
            args: vec!["-L".into(), "/usr/riscv64-linux-gnu".into()],
        };
        let (program, args) = l.command(Path::new("/tmp/unit"));
        assert_eq!(program, "qemu-riscv64");
        assert_eq!(args, vec!["-L", "/usr/riscv64-linux-gnu", "/tmp/unit"]);
    }
}
