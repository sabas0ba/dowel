//! テストの実行。
//!
//! `test` 種別のターゲットはビルドすると実行ファイルになる。ここはそれを起動して
//! 終了状態を集めるだけの層である。テストハーネスは持たない。
//! 「終了状態 0 なら成功」という C の慣習に従い、枠組みは利用者の側に委ねる。
//!
//! 実行の直前に1箇所だけ噛ませてある [`Launcher`] が、ランナー抽象
//! （qemu / SSH / 実機、docs/30-devexp.md 1節）の差し込み口になる。
//! クロス実行では成果物を直接起動できないため、そこだけが変わる。

use crate::plan::Plan;
use dowel_model::{Session, TargetId};
use dowel_support::{log_debug, log_trace};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// 成果物を起動するコマンドを組み立てる。
///
/// 既定は「成果物をそのまま起動する」。ターゲットトリプルごとの実行ラッパは
/// 未実装であり、それを差し込むのがこの型の役目になる。
pub struct Launcher;

impl Launcher {
    pub fn for_config(_cfg: &dowel_eval::Config) -> Launcher {
        // 構成を受け取る形にしてあるのは、ランナーの選択がターゲットトリプルに
        // 依るためである。現時点では選択肢が1つしかない。
        Launcher
    }

    /// `binary` を起動するためのプログラムと引数。
    pub fn command(&self, binary: &Path) -> (String, Vec<String>) {
        (binary.display().to_string(), Vec::new())
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

/// 与えられたテストターゲットを順に起動する。
///
/// `capture` が偽のときは子プロセスの出力をそのまま素通しする。
pub fn run(
    sess: &Session,
    plan: &Plan,
    launcher: &Launcher,
    targets: &[TargetId],
    capture: bool,
) -> Vec<Outcome> {
    let _phase = dowel_support::log::Phase::start("test");
    let mut out = Vec::new();
    for &tid in targets {
        let label = sess.label(tid);
        let Some(binary) = plan.artifacts.get(&tid).cloned() else {
            out.push(Outcome {
                target: tid,
                label: label.clone(),
                binary: PathBuf::new(),
                status: None,
                passed: false,
                duration_ms: 0,
                stdout: String::new(),
                stderr: String::new(),
                launch_error: Some("no artifact was planned for this target".into()),
            });
            continue;
        };

        // 作業ディレクトリはパッケージルート。テストが読む固定資産の相対パスが、
        // マニフェストに書いたものと同じ基準で解決されるようにする。
        let cwd = sess.package(sess.target(tid).package).root.clone();
        let (program, args) = launcher.command(&binary);
        log_debug!("running {label}");
        log_trace!("  {} (cwd {})", program, cwd.display());

        let mut cmd = Command::new(&program);
        cmd.args(&args).current_dir(&cwd);
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

        out.push(match result {
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
        });
    }
    out
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

    #[test]
    fn the_default_launcher_starts_the_artifact_directly() {
        let l = Launcher::for_config(&dowel_eval::Config::host_default());
        let (program, args) = l.command(Path::new("/tmp/unit"));
        assert_eq!(program, "/tmp/unit");
        assert!(args.is_empty());
    }
}
