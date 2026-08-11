//! ベンチマークの計測（[ADR-0025](../../../docs/adr/0025-bench-wall-clock.md)）。
//!
//! 測るのは**プロセス全体の壁時計**である。dowel は計測の枠組みを課さない
//! ——テストと同じ判断だが、理由は1つ深い。関数単位の計測は反復回数や
//! 最適化の抑止と不可分であり、それを課すことは枠組みを課すことに他ならない。
//! プロセスの起動から終了までなら、どのバイナリにも同じ物差しが当たる。
//!
//! 報告は min と median である（`scripts/measure-startup.py` と同じ流儀）。
//! min は「機械が邪魔しなかったときの実力」、median は「普段の見え方」。
//! mean は外れ値に引かれるので出さない。
//!
//! 判定はしない。速さの合否は機械と閾値の選び方に依存し、その選択は
//! 利用者のものである。dowel が失敗と呼ぶのは、走らせられなかったこと
//! （非零終了・シグナル・時間切れ・起動失敗）だけである。

use crate::testing::{capture_run, Job};
use dowel_model::TargetId;
use dowel_support::{log_debug, log_trace};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

/// 既定の反復回数。
///
/// 1回では機械の揺れと区別できず、増やすほど遅い計測が苦しくなる。
/// 10 は「min が意味を持ち始める」側に倒した値であり、`--iterations` で
/// 変えられる。
pub const DEFAULT_ITERATIONS: usize = 10;

/// 1つの計測対象の結果。
#[derive(Debug)]
pub struct Measurement {
    pub target: TargetId,
    /// ターゲットのラベル。事例の名前は含まない
    pub target_label: String,
    /// 事例の名前。事例を持たないターゲットでは `None`
    pub case: Option<String>,
    pub binary: PathBuf,
    pub args: Vec<String>,
    /// 実際に測った回数。失敗で打ち切った場合は要求より少ない
    pub runs: usize,
    /// マイクロ秒。壁時計であり、CPU 時間ではない
    pub min_us: u128,
    pub median_us: u128,
    pub max_us: u128,
    /// 走らせられなかった理由。計測の失敗であって、遅さではない
    pub failure: Option<String>,
}

impl Measurement {
    /// 印字される綴り。`<パッケージ>:<ターゲット>[/<事例>]`
    pub fn label(&self) -> String {
        match &self.case {
            Some(c) => format!("{}/{c}", self.target_label),
            None => self.target_label.clone(),
        }
    }

    /// 1行の結果表示。
    pub fn summary_line(&self) -> String {
        match &self.failure {
            Some(_) => format!("bench {} ... FAILED", self.label()),
            None => format!(
                "bench {} ... min {}  median {}  ({} runs)",
                self.label(),
                render_us(self.min_us),
                render_us(self.median_us),
                self.runs
            ),
        }
    }
}

/// マイクロ秒を人の読む単位で。1ms 未満は µs、1s 以上は s。
fn render_us(us: u128) -> String {
    if us < 1_000 {
        format!("{us}µs")
    } else if us < 1_000_000 {
        format!("{}.{:02}ms", us / 1_000, (us % 1_000) / 10)
    } else {
        format!("{}.{:02}s", us / 1_000_000, (us % 1_000_000) / 10_000)
    }
}

/// 与えられた対象を測る。
///
/// 常に逐次である。計測は静かな機械を前提とし、並べて走らせた数字は
/// 互いの雑音になる——`--test-jobs` に相当する選択肢を設けないのは
/// このためである。
///
/// 1回でも走らせられなかった対象は、その時点で打ち切って失敗として
/// 報告する。壊れた計測を最後まで回しても得るものが無い。
pub fn measure(jobs: &[Job], iterations: usize) -> Vec<Measurement> {
    let _phase = dowel_support::log::Phase::start("bench");
    let iterations = iterations.max(1);
    log_debug!("measuring {} benchmark(s), {iterations} run(s) each", jobs.len());
    jobs.iter().map(|job| measure_one(job, iterations)).collect()
}

fn measure_one(job: &Job, iterations: usize) -> Measurement {
    let mut m = Measurement {
        target: job.target,
        target_label: job.target_label.clone(),
        case: job.case.clone(),
        binary: job.binary.clone().unwrap_or_default(),
        args: job.args.clone(),
        runs: 0,
        min_us: 0,
        median_us: 0,
        max_us: 0,
        failure: None,
    };
    if job.binary.is_none() {
        m.failure = Some("no artifact was planned for this target".into());
        return m;
    }
    if !job.cwd.is_dir() {
        m.failure = Some(format!("the working directory does not exist: {}", job.cwd.display()));
        return m;
    }
    // 対象機がビルド機のファイルシステムを見られない場合の転送。
    // 計測の外で1度だけ行う——転送の時間は計測の対象ではない。
    if let Err(e) = crate::testing::transfer(job) {
        m.failure = Some(format!("could not transfer the artifact: {e}"));
        return m;
    }

    log_debug!("measuring {}", m.label());
    let mut samples: Vec<u128> = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let mut cmd = Command::new(&job.program);
        cmd.args(&job.args).current_dir(&job.cwd);
        for (k, v) in &job.env {
            cmd.env(k, v);
        }
        let start = Instant::now();
        let result = capture_run(&mut cmd, job.timeout);
        let elapsed = start.elapsed().as_micros();
        // 出力は読むが使わない。読まないとパイプが詰まり、書く側ごと
        // 計測が止まる。
        match result {
            Err(e) => {
                m.failure = Some(format!("cannot start `{}`: {e}", job.program));
                return m;
            }
            Ok((_, true, _, _)) => {
                m.failure = Some(format!("run {} timed out and was killed", i + 1));
                return m;
            }
            Ok((status, false, _, stderr)) if !status.success() => {
                let how = match status.code() {
                    Some(c) => format!("exited with status {c}"),
                    None => "was terminated by a signal".to_string(),
                };
                let tail = stderr.trim_end();
                m.failure = Some(match tail.is_empty() {
                    true => format!("run {} {how}", i + 1),
                    false => format!("run {} {how}\n{tail}", i + 1),
                });
                return m;
            }
            Ok(_) => {}
        }
        log_trace!("  run {}: {elapsed}µs", i + 1);
        samples.push(elapsed);
        m.runs = i + 1;
    }

    samples.sort_unstable();
    m.min_us = samples[0];
    m.max_us = samples[samples.len() - 1];
    // 偶数個なら中央2つの平均。`statistics.median` と同じ定義にする。
    let mid = samples.len() / 2;
    m.median_us =
        if samples.len() % 2 == 1 { samples[mid] } else { (samples[mid - 1] + samples[mid]) / 2 };
    m
}

/// 機械可読な結果。1件1行の JSON。時間は µs の整数で持つ——
/// 小数の ms を作るのは読む側の整形の仕事である。
pub fn render_json(m: &Measurement) -> String {
    let mut w = dowel_support::json::JsonWriter::new();
    w.begin_object();
    w.field_str("kind", "bench-result");
    w.field_str("target", &m.target_label);
    match &m.case {
        Some(c) => w.field_str("case", c),
        None => w.key("case").null(),
    };
    w.field_str("label", &m.label());
    w.field_str("binary", &m.binary.display().to_string());
    w.field_strs("args", m.args.iter().map(|s| s.as_str()));
    w.field_u64("runs", m.runs as u64);
    match m.failure {
        None => {
            w.key("min_us").u64(m.min_us as u64);
            w.key("median_us").u64(m.median_us as u64);
            w.key("max_us").u64(m.max_us as u64);
            w.key("failure").null();
        }
        Some(ref e) => {
            // 数字は出さない。失敗の回の前までの数字は「揃った計測」では
            // なく、混ぜて読まれるのが一番悪い。
            w.key("min_us").null();
            w.key("median_us").null();
            w.key("max_us").null();
            w.field_str("failure", e);
        }
    }
    w.end_object();
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measurement(failure: Option<&str>) -> Measurement {
        Measurement {
            target: TargetId(0),
            target_label: "pkg:b".into(),
            case: Some("small".into()),
            binary: PathBuf::from("/b/bin/b"),
            args: vec!["small".into()],
            runs: 10,
            min_us: 950,
            median_us: 1234,
            max_us: 20_000,
            failure: failure.map(|s| s.to_string()),
        }
    }

    #[test]
    fn the_summary_reads_in_human_units() {
        let line = measurement(None).summary_line();
        assert_eq!(line, "bench pkg:b/small ... min 950µs  median 1.23ms  (10 runs)");
        assert!(measurement(Some("x")).summary_line().contains("FAILED"));
    }

    #[test]
    fn the_json_keeps_microseconds_and_separates_target_and_case() {
        let json = render_json(&measurement(None));
        assert!(json.contains(r#""kind":"bench-result""#), "{json}");
        assert!(json.contains(r#""target":"pkg:b""#), "{json}");
        assert!(json.contains(r#""case":"small""#), "{json}");
        assert!(json.contains(r#""label":"pkg:b/small""#), "{json}");
        assert!(json.contains(r#""min_us":950"#), "{json}");
        assert!(json.contains(r#""median_us":1234"#), "{json}");
        assert!(json.contains(r#""failure":null"#), "{json}");
    }

    #[test]
    fn a_failed_measurement_reports_no_numbers() {
        // 途中までの数字は「揃った計測」ではない。混ぜて読まれるのが一番悪い。
        let json = render_json(&measurement(Some("run 3 exited with status 1")));
        assert!(json.contains(r#""min_us":null"#), "{json}");
        assert!(json.contains(r#""failure":"run 3 exited with status 1""#), "{json}");
    }

    #[test]
    fn durations_render_in_the_right_unit() {
        assert_eq!(render_us(7), "7µs");
        assert_eq!(render_us(999), "999µs");
        assert_eq!(render_us(1_000), "1.00ms");
        assert_eq!(render_us(999_990), "999.99ms");
        assert_eq!(render_us(2_345_678), "2.34s");
    }
}
