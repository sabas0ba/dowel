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
use dowel_eval::{Config, Data, Value};
use dowel_model::{Session, TargetId};
use dowel_support::{log_debug, log_trace};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 時間切れを見張る間隔。
///
/// std には待ち時間つきの `wait` が無いため、`try_wait` を回す。10ms は
/// 「テストの時間として誤差になる」側に倒した値であり、走らせている間の
/// 起床回数は毎分6000回——1つのプロセスにとって無視できる。
const POLL: Duration = Duration::from_millis(10);

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
    /// 成果物の転送。SSH やシリアル経由のように、対象機が
    /// ビルド機のファイルシステムを見られない場合に設定する
    transfer: Option<Transfer>,
}

/// 成果物を対象機へ運ぶ手順（docs/adr/0008-runner-transfer.md）。
///
/// パスはマニフェストに書かせず、実装が末尾に付け足す。
/// `transfer` には `<ローカルの成果物> <転送先>` を、
/// `command` / `args` には転送先のパスを付ける。
/// 文字列補間を導入しないための形である。
#[derive(Clone, Debug)]
struct Transfer {
    /// 転送コマンドとその固定引数
    command: Vec<String>,
    /// 対象機側のディレクトリ
    remote_dir: String,
    /// 転送先のホスト。`<host>:<path>` の形を作るためにだけ使う
    host: Option<String>,
}

impl Transfer {
    /// 対象機での成果物のパス。
    fn remote_path(&self, binary: &Path) -> String {
        let name = binary.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        format!("{}/{name}", self.remote_dir.trim_end_matches('/'))
    }

    /// 転送コマンドの完全な引数。末尾は `<ローカル> <転送先>`。
    fn command_for(&self, binary: &Path) -> (String, Vec<String>) {
        let remote = self.remote_path(binary);
        let destination = match &self.host {
            Some(h) => format!("{h}:{remote}"),
            None => remote,
        };
        let mut parts = self.command.iter().skip(1).cloned().collect::<Vec<_>>();
        parts.push(binary.display().to_string());
        parts.push(destination);
        (self.command[0].clone(), parts)
    }
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
            return (Launcher::direct(), diags);
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
        let str_prop = |name: &str| {
            runner
                .prop(name)
                .and_then(|v| dowel_eval::specialize(v, cfg))
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        };
        let transfer_cmd = runner
            .prop("transfer")
            .and_then(|v| dowel_eval::specialize(v, cfg))
            .map(|v| string_list(&v))
            .unwrap_or_default();
        // 組み合わせの妥当性は読み込み時に検証済み。ここは具体化の結果を見る。
        let transfer = match (transfer_cmd.is_empty(), str_prop("remote_dir")) {
            (false, Some(remote_dir)) => {
                log_debug!("runner for `{}` transfers via {}", cfg.target, transfer_cmd.join(" "));
                Some(Transfer { command: transfer_cmd, remote_dir, host: str_prop("host") })
            }
            _ => None,
        };

        match program {
            Some(program) => {
                log_debug!("runner for `{}`: {program} {}", cfg.target, args.join(" "));
                (Launcher { program: Some(program), args, transfer }, diags)
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
                (Launcher::direct(), diags)
            }
        }
    }

    /// ラッパを持たない起動器。ランナーを要さない経路と試験のために使う。
    pub fn direct() -> Launcher {
        Launcher { program: None, args: Vec::new(), transfer: None }
    }

    /// `binary` を起動するためのプログラムと引数。
    ///
    /// 転送を伴う場合、渡すのは対象機側のパスである。ローカルのパスを渡すと
    /// 対象機に存在しないファイルを起動しようとする。
    pub fn command(&self, binary: &Path) -> (String, Vec<String>) {
        let target_path = match &self.transfer {
            Some(t) => t.remote_path(binary),
            None => binary.display().to_string(),
        };
        match &self.program {
            None => (target_path, Vec::new()),
            Some(program) => {
                let mut args = self.args.clone();
                args.push(target_path);
                (program.clone(), args)
            }
        }
    }

    /// 起動前に実行する転送コマンド。転送を伴わない場合は `None`。
    pub fn transfer_command(&self, binary: &Path) -> Option<(String, Vec<String>)> {
        self.transfer.as_ref().map(|t| t.command_for(binary))
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
    /// ターゲットのラベル。事例の名前は含まない
    pub target_label: String,
    /// 事例の名前。事例を持たないターゲットでは `None`
    pub case: Option<String>,
    pub binary: PathBuf,
    /// 実行ファイルに渡した引数
    pub args: Vec<String>,
    /// この事例が答える名前（`--label` が引く）
    pub labels: Vec<String>,
    /// 宣言されていた制限時間
    pub timeout: Option<Duration>,
    /// プロセスを起動できなかった場合と、シグナルで終わった場合は `None`
    pub status: Option<i32>,
    /// シグナルで終わった場合のその番号。
    ///
    /// 時間切れで**こちらが**殺した場合は入れない。それはプログラムの
    /// 終わり方ではなく、`timed_out` が述べている（issue #88）。
    pub signal: Option<i32>,
    /// 時間切れで打ち切った。`status` は殺した結果であって、テストの答ではない
    pub timed_out: bool,
    /// 非零の終了を期待していた（`should_fail`）
    pub should_fail: bool,
    pub passed: bool,
    pub duration_ms: u128,
    /// `capture` が真のときのみ中身を持つ
    pub stdout: String,
    pub stderr: String,
    /// 起動そのものに失敗した理由
    pub launch_error: Option<String>,
}

impl Outcome {
    /// 仕事の側から決まる欄を写した、まだ走っていない結果。
    ///
    /// 結果の欄だけを後から埋める。欄が増えるたびに全ての生成箇所を直すと、
    /// どこかが既定のまま残る。
    fn of(job: &Job) -> Outcome {
        Outcome {
            target: job.target,
            target_label: job.target_label.clone(),
            case: job.case.clone(),
            binary: job.binary.clone().unwrap_or_default(),
            args: job.args.clone(),
            labels: job.labels.clone(),
            timeout: job.timeout,
            status: None,
            signal: None,
            timed_out: false,
            should_fail: job.should_fail,
            passed: false,
            duration_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            launch_error: None,
        }
    }

    /// 印字される綴り。`<パッケージ>:<ターゲット>[/<事例>]`
    pub fn label(&self) -> String {
        label_of(&self.target_label, self.case.as_deref())
    }

    /// 1行の結果表示。`test <ラベル> ... ok (12ms)`
    pub fn summary_line(&self) -> String {
        let verdict = if self.passed { "ok" } else { "FAILED" };
        format!("test {} ... {verdict} ({}ms)", self.label(), self.duration_ms)
    }

    /// 失敗の理由を1行で。成功時は `None`。
    pub fn failure_reason(&self) -> Option<String> {
        if self.passed {
            return None;
        }
        if self.timed_out {
            return Some("timed out and was killed".to_string());
        }
        if let Some(sig) = self.signal {
            // 異常な終わり方は、期待された失敗ではない（issue #88）。
            // 落ちることを期待して `should_fail` を書く者はいない。
            let crash = format!("killed by signal {sig}{}", named(sig));
            return Some(match self.should_fail {
                true => format!("{crash}; `should_fail` expects a nonzero exit, not a crash"),
                false => crash,
            });
        }
        // 期待した失敗が起きなかった場合、「状態0で終了した」だけでは
        // なぜ失敗なのか読めない。期待の側を述べる。
        if self.should_fail && self.status == Some(0) {
            return Some("exited with status 0, but `should_fail` expects a nonzero exit".into());
        }
        Some(match (&self.launch_error, self.status) {
            (Some(e), _) => format!("could not start the test binary: {e}"),
            (None, Some(code)) => format!("exited with status {code}"),
            (None, None) => "terminated by a signal".to_string(),
        })
    }
}

fn label_of(target: &str, case: Option<&str>) -> String {
    match case {
        Some(c) => format!("{target}/{c}"),
        None => target.to_string(),
    }
}

/// シグナル番号に添える名前。` (SIGSEGV)` の形で返す。
///
/// 番号が系によって違うものは名前を付けない。`SIGBUS` は Linux で 7、
/// macOS で 10 である——取り違えた名前は、番号だけより悪い。
fn named(sig: i32) -> String {
    const NAMES: &[(i32, &str)] = &[
        (1, "SIGHUP"),
        (2, "SIGINT"),
        (3, "SIGQUIT"),
        (4, "SIGILL"),
        (5, "SIGTRAP"),
        (6, "SIGABRT"),
        (8, "SIGFPE"),
        (9, "SIGKILL"),
        (11, "SIGSEGV"),
        (13, "SIGPIPE"),
        (14, "SIGALRM"),
        (15, "SIGTERM"),
    ];
    match NAMES.iter().find(|(n, _)| *n == sig) {
        Some((_, name)) => format!(" ({name})"),
        None => String::new(),
    }
}

/// 終了状態からシグナル番号を取り出す。
///
/// unix 以外では常に `None`。`ExitStatus::code` が `None` を返す理由は
/// 系によって違い、そこを跨いで述べられることは無い。
fn signal_of(status: &ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::signal(status)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// 1本のテストを起動するために必要な情報。
///
/// `Session` から分離しているのは、並列実行の作業スレッドがモデルを参照しない
/// ようにするためである。`Session` は増分エンジンのメモ表を保持しており、
/// スレッド間で共有できない。起動対象の決定は逐次に行い、スレッドは起動のみを担う。
#[derive(Clone, Debug)]
pub struct Job {
    pub target: TargetId,
    /// ターゲットのラベル。`<パッケージ>:<ターゲット>`
    pub target_label: String,
    /// 事例の名前。持たないターゲットでは `None`
    pub case: Option<String>,
    /// 計画に成果物が無い場合は `None`
    pub binary: Option<PathBuf>,
    /// 起動時の作業ディレクトリ。既定は宣言したパッケージの根で、
    /// 事例の `cwd` で変えられる（issue #95）
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    /// この事例だけに設定する環境変数
    pub env: Vec<(String, String)>,
    /// 過ぎたら殺す。宣言が無ければ待ち続ける
    pub timeout: Option<Duration>,
    /// 非零の終了を期待する
    pub should_fail: bool,
    /// `--label` が引く名前
    pub labels: Vec<String>,
    /// 起動前に走らせる転送コマンド。対象機がビルド機の
    /// ファイルシステムを見られない場合にのみ入る
    pub transfer: Option<(String, Vec<String>)>,
    /// 実行ファイル自身に事例を列挙させる宣言（ADR-0023）。
    /// これがある仕事は「まだ事例が分かっていない1件」であり、
    /// [`discover`] が本当の仕事の列に展開する
    pub harness: Option<Harness>,
}

impl Job {
    /// 表示と `--failed` の鍵。`<パッケージ>:<ターゲット>[/<事例>]`
    ///
    /// 綴りを組み立てる場所を1つに閉じる。持ち回ると、目標と事例を分けて
    /// 報告する側（issue #100）が同じ規則を2度書くことになる。
    pub fn label(&self) -> String {
        label_of(&self.target_label, self.case.as_deref())
    }
}

/// 実行ファイルへの尋ね方（ADR-0023）。dowel は枠組みを1つも知らない。
#[derive(Clone, Debug)]
pub struct Harness {
    /// 事例の名前を1行ずつ書き出させる引数
    pub list: Vec<String>,
    /// 1件だけ走らせるときに、名前の**前**に置く引数
    pub run: Vec<String>,
}

/// 走らせるものを1つずつ数え上げる。
///
/// `[test.<name>.cases]` を宣言していないターゲットは、それ自身が1件である
/// （従来の形）。宣言していれば、同じ実行ファイルを事例の数だけ起動する。
///
/// 具体化はここで行う。事例の引数や時間切れは構成で変えられる——
/// クロスでだけ長い時間を許す、といった形が書ける。
pub fn plan_jobs(
    sess: &Session,
    plan: &Plan,
    launcher: &Launcher,
    targets: &[TargetId],
    cfg: &Config,
) -> Vec<Job> {
    let mut out = Vec::new();
    for &tid in targets {
        let binary = plan.artifacts.get(&tid).cloned();
        let (program, base_args) = match &binary {
            Some(b) => launcher.command(b),
            None => (String::new(), Vec::new()),
        };
        let transfer = binary.as_ref().and_then(|b| launcher.transfer_command(b));
        // 作業ディレクトリはパッケージルート。テストが読む固定資産の相対パスが、
        // マニフェストに書いたものと同じ基準で解決されるようにする。
        let cwd = sess.package(sess.target(tid).package).root.clone();
        let cfg = cfg.for_package(&sess.package(sess.target(tid).package).name);
        let base = Job {
            target: tid,
            target_label: sess.label(tid),
            case: None,
            binary: binary.clone(),
            cwd: cwd.clone(),
            program: program.clone(),
            args: base_args.clone(),
            env: Vec::new(),
            timeout: None,
            should_fail: false,
            labels: Vec::new(),
            transfer: transfer.clone(),
            harness: None,
        };
        // ハーネスの宣言があれば、事例はまだ分かっていない。ここでは
        // 「尋ねるべき1件」として積み、`discover` が展開する。
        if let Some(decl) = &sess.target(tid).harness {
            let field =
                |name: &str| decl.fields.get(name).and_then(|v| dowel_eval::specialize(v, &cfg));
            let mut job = base.clone();
            job.env = pairs(field("env").as_ref());
            job.timeout = seconds(field("timeout").as_ref());
            job.labels = strings(field("labels").as_ref());
            job.harness = Some(Harness {
                list: strings(field("list").as_ref()),
                run: strings(field("run").as_ref()),
            });
            out.push(job);
            continue;
        }

        let cases = &sess.target(tid).cases;
        if cases.is_empty() {
            out.push(base);
            continue;
        }
        for case in cases {
            // 事例そのものが条件付きでありうる。偽なら、この構成にその事例は
            // 存在しない（issue #92）。
            let Some(concrete) = dowel_eval::specialize(&case.value, &cfg) else {
                log_trace!("  case {}/{} is not registered here", base.target_label, case.name);
                continue;
            };
            let Data::Map(fields) = &concrete.data else { continue };
            let field = |name: &str| fields.get(name).cloned();
            let mut job = base.clone();
            job.case = Some(case.name.clone());
            job.args.extend(strings(field("args").as_ref()));
            job.env = pairs(field("env").as_ref());
            job.timeout = seconds(field("timeout").as_ref());
            job.should_fail =
                matches!(field("should_fail").map(|v| v.data), Some(Data::Bool(true)));
            job.labels = strings(field("labels").as_ref());
            // 作業ディレクトリを述べた事例は、そこで走る（issue #95）。
            if let Some(dir) = field("cwd").as_ref().and_then(|v| directory(v, sess, tid)) {
                job.cwd = dir;
            }
            out.push(job);
        }
    }
    out
}

/// 事例の `cwd` を実在の場所にする（issue #95）。
///
/// 基準はそれを書いたパッケージの根である。`dir()` は書いた場所からの
/// 相対であり、事例を持つターゲットが別のパッケージから読まれても意味が
/// 変わらない。
fn directory(v: &Value, sess: &Session, tid: TargetId) -> Option<PathBuf> {
    let Data::Path(p) = &v.data else { return None };
    if p.base != dowel_eval::value::PathBase::Package {
        return None;
    }
    let root = v
        .prov
        .nearest_site()
        .and_then(|s| sess.package_of_file(s.file))
        .unwrap_or(sess.target(tid).package);
    Some(sess.package(root).root.join(&p.rel))
}

fn seconds(v: Option<&Value>) -> Option<Duration> {
    match v.map(|v| &v.data) {
        Some(Data::Int(n)) if *n > 0 => Some(Duration::from_secs(*n as u64)),
        _ => None,
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    let Some(Data::List(items)) = v.map(|v| &v.data) else { return Vec::new() };
    items.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect()
}

fn pairs(v: Option<&Value>) -> Vec<(String, String)> {
    let Some(Data::Map(map)) = v.map(|v| &v.data) else { return Vec::new() };
    map.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect()
}

/// ハーネスの宣言を持つ仕事を、実際の事例へ展開する（ADR-0023）。
///
/// 実行ファイルに尋ねるので、外部プロセスが走る。計画の段ではなくここで行う
/// のは、尋ねる相手がまだ組み上がっていないためである。
///
/// 尋ねられなかった場合は、0件成功にせず失敗として報告する。列挙できない
/// ことと事例が無いことは別である——黙って0件にすると、試験が消えたことに
/// 誰も気づかない。
pub fn discover(jobs: Vec<Job>) -> (Vec<Job>, Vec<Outcome>) {
    let mut out = Vec::new();
    let mut failures = Vec::new();
    for job in jobs {
        let Some(harness) = job.harness.clone() else {
            out.push(job);
            continue;
        };
        match list_cases(&job, &harness) {
            Ok(names) if names.is_empty() => {
                failures.push(discovery_failed(&job, "the harness listed no cases".to_string()))
            }
            Ok(names) => {
                log_debug!("{} lists {} case(s)", job.label(), names.len());
                for name in names {
                    let mut case = job.clone();
                    case.case = Some(name.clone());
                    case.args.extend(harness.run.iter().cloned());
                    case.args.push(name);
                    case.harness = None;
                    out.push(case);
                }
            }
            Err(e) => failures.push(discovery_failed(&job, e)),
        }
    }
    (out, failures)
}

/// 列挙に失敗した仕事を、そのターゲットの1件の失敗として報告する。
fn discovery_failed(job: &Job, why: String) -> Outcome {
    Outcome {
        // 列挙できていないので、この失敗は事例ではなくターゲットのものである。
        should_fail: false,
        launch_error: Some(format!("could not list the cases: {why}")),
        ..Outcome::of(job)
    }
}

/// 実行ファイルに事例の名前を尋ねる。
///
/// 出力は1行1件。空行と `#` で始まる行は読み飛ばす。それ以上の解釈はしない——
/// 解釈を足すと、その形を出す枠組みだけが使える形になる。
fn list_cases(job: &Job, harness: &Harness) -> Result<Vec<String>, String> {
    if job.binary.is_none() {
        return Err("no artifact was planned for this target".into());
    }
    transfer(job).map_err(|e| format!("could not transfer the artifact: {e}"))?;

    let mut cmd = Command::new(&job.program);
    cmd.args(&job.args).args(&harness.list).current_dir(&job.cwd);
    for (k, v) in &job.env {
        cmd.env(k, v);
    }
    log_debug!("listing the cases of {}", job.label());
    log_trace!("  {} {}", job.program, harness.list.join(" "));

    let (status, timed_out, stdout, stderr) = capture_run(&mut cmd, job.timeout)
        .map_err(|e| format!("cannot start `{}`: {e}", job.program))?;
    if timed_out {
        return Err("the listing timed out".into());
    }
    if !status.success() {
        let tail = stderr.trim_end();
        return Err(match status.code() {
            Some(c) if tail.is_empty() => format!("the listing exited with status {c}"),
            Some(c) => format!("the listing exited with status {c}\n{tail}"),
            None => "the listing was terminated by a signal".into(),
        });
    }
    Ok(stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect())
}

/// 与えられたテストターゲットを起動する。
///
/// 戻り値は要求順。`fail_fast` で打ち切った場合、起動しなかったものは含まれない。
/// 呼び出し側は要求数との差から未実行の件数を得る。
pub fn run(planned: &[Job], opts: &RunOptions) -> Vec<Outcome> {
    let _phase = dowel_support::log::Phase::start("test");
    let jobs = opts.jobs.max(1).min(planned.len().max(1));
    log_debug!("running {} tests with {jobs} job(s)", planned.len());
    for j in planned {
        log_trace!(
            "  planned {}: {} (cwd {})",
            j.label(),
            if j.program.is_empty() { "<no artifact>" } else { &j.program },
            j.cwd.display()
        );
        if let Some((program, args)) = &j.transfer {
            log_trace!("    transfer: {program} {}", args.join(" "));
        }
    }

    if jobs == 1 {
        let mut out = Vec::new();
        for job in planned {
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

/// 成果物を対象機へ転送する。失敗した理由をそのまま返す。
fn transfer(job: &Job) -> Result<(), String> {
    let Some((program, args)) = &job.transfer else { return Ok(()) };
    log_debug!("transferring the artifact for {}", job.label());
    log_trace!("  {program} {}", args.join(" "));
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot start `{program}`: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    // 転送の失敗はテストの失敗ではない。理由をそのまま見せる。
    Err(format!(
        "`{program}` exited with {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).trim_end()
    ))
}

fn run_one(job: &Job, capture: bool) -> Outcome {
    let Job { binary, cwd, program, args, .. } = job;
    let (label, cwd) = (job.label(), cwd.clone());
    let failed = |why: String| Outcome { launch_error: Some(why), ..Outcome::of(job) };
    if binary.is_none() {
        return failed("no artifact was planned for this target".into());
    }

    if let Err(e) = transfer(job) {
        return failed(format!("could not transfer the artifact: {e}"));
    }
    // 走る場所が無ければ、起動の失敗として述べる。`spawn` が返す
    // `No such file or directory` は、実行ファイルが無いようにも読める。
    if !cwd.is_dir() {
        return failed(format!("the working directory does not exist: {}", cwd.display()));
    }

    log_debug!("running {label}");
    log_trace!("  {program} (cwd {})", cwd.display());
    if let Some(t) = job.timeout {
        log_trace!("  timeout {}s", t.as_secs());
    }

    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(&cwd);
    for (k, v) in &job.env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let result = if capture {
        capture_run(&mut cmd, job.timeout)
    } else {
        pass_through(&mut cmd, job.timeout)
    };
    let duration_ms = start.elapsed().as_millis();

    match result {
        Ok((status, timed_out, stdout, stderr)) => {
            // 殺したのがこちらなら、シグナルはプログラムの終わり方ではない。
            let signal = if timed_out { None } else { signal_of(&status) };
            // 時間切れも異常終了も、終了状態が何であれ失敗である。
            // `should_fail` が述べているのは「非零で終了すること」であって、
            // 落ちることではない（issue #88）。
            let passed = if timed_out || signal.is_some() {
                false
            } else if job.should_fail {
                !status.success()
            } else {
                status.success()
            };
            Outcome {
                status: status.code(),
                signal,
                timed_out,
                passed,
                duration_ms,
                stdout,
                stderr,
                ..Outcome::of(job)
            }
        }
        Err(e) => Outcome { duration_ms, ..failed(e.to_string()) },
    }
}

/// 出力を捕まえて走らせる。
///
/// `Command::output` を使わないのは、時間切れを見張れないためである。
/// パイプは別のスレッドで読む。読まずに待つと、子の書き込みがパイプの
/// 緩衝を埋めた時点で両者が止まる。
fn capture_run(
    cmd: &mut Command,
    timeout: Option<Duration>,
) -> std::io::Result<(ExitStatus, bool, String, String)> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut out_pipe = child.stdout.take().expect("stdout was piped");
    let mut err_pipe = child.stderr.take().expect("stderr was piped");
    std::thread::scope(|scope| {
        let out = scope.spawn(move || {
            let mut s = Vec::new();
            let _ = out_pipe.read_to_end(&mut s);
            s
        });
        let err = scope.spawn(move || {
            let mut s = Vec::new();
            let _ = err_pipe.read_to_end(&mut s);
            s
        });
        let waited = wait_until(&mut child, timeout);
        // 殺した後もパイプは閉じるので、読み手は必ず終わる。
        let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).to_string();
        let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).to_string();
        waited.map(|(s, t)| (s, t, stdout, stderr))
    })
}

/// 出力を素通しして走らせる。読む相手がいないので、待つだけでよい。
fn pass_through(
    cmd: &mut Command,
    timeout: Option<Duration>,
) -> std::io::Result<(ExitStatus, bool, String, String)> {
    let mut child = cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn()?;
    let (status, timed_out) = wait_until(&mut child, timeout)?;
    Ok((status, timed_out, String::new(), String::new()))
}

/// 終了を待つ。`timeout` を過ぎたら殺す。
///
/// 殺すのは子だけである。孫は残る——`kill` は直接の子にしか届かない。
/// プロセスグループごと殺すには std の外へ出る必要があり、そこまでは踏み込まない
/// （docs/60-cli.md に明記する）。
fn wait_until(child: &mut Child, timeout: Option<Duration>) -> std::io::Result<(ExitStatus, bool)> {
    let Some(limit) = timeout else { return Ok((child.wait()?, false)) };
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if start.elapsed() >= limit {
            log_debug!("  timed out after {}s; killing", limit.as_secs());
            let _ = child.kill();
            return Ok((child.wait()?, true));
        }
        std::thread::sleep(POLL);
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
            self.results.insert(o.label(), o.passed);
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
///
/// 目標と事例は別の欄に置く（issue #100）。1つの欄に `<目標>/<事例>` と
/// 詰めると、読む側は最後の `/` で割るしかない——ハーネスが列挙した名前は
/// `/` を含みうるので、その推測は当たらない。
pub fn render_json(o: &Outcome) -> String {
    let mut w = dowel_support::json::JsonWriter::new();
    w.begin_object();
    w.field_str("kind", "test-result");
    w.field_str("target", &o.target_label);
    match &o.case {
        Some(c) => w.field_str("case", c),
        None => w.key("case").null(),
    };
    // 印字される綴り。組み立て直さずに済むように、そのまま出す。
    w.field_str("label", &o.label());
    w.field_strs("labels", o.labels.iter().map(|s| s.as_str()));
    w.field_bool("should_fail", o.should_fail);
    match o.timeout {
        Some(t) => w.key("timeout").u64(t.as_secs()),
        None => w.key("timeout").null(),
    };
    w.field_str("binary", &o.binary.display().to_string());
    w.field_strs("args", o.args.iter().map(|s| s.as_str()));
    w.field_bool("passed", o.passed);
    // 時間切れは終了状態からは読めない。殺した結果が入るだけである。
    w.field_bool("timed_out", o.timed_out);
    match o.status {
        Some(c) => w.key("exit_status").i64(c as i64),
        None => w.key("exit_status").null(),
    };
    // `exit_status` が無い理由は1つではない。時間切れ、シグナル、起動の失敗
    // ——読む側が区別できるように、それぞれに欄を持たせる（issue #88）。
    match o.signal {
        Some(s) => w.key("signal").i64(s as i64),
        None => w.key("signal").null(),
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
            target_label: "pkg:unit".into(),
            case: None,
            binary: PathBuf::from("/tmp/unit"),
            args: Vec::new(),
            labels: Vec::new(),
            timeout: None,
            status,
            signal: None,
            timed_out: false,
            should_fail: false,
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
    fn json_names_the_target_and_the_case_separately() {
        // 1つの欄に詰めると、読む側は最後の `/` で割ることになる（issue #100）。
        let mut o = outcome(true, Some(0), None);
        o.case = Some("parse/deep".into());
        o.labels = vec!["slow".into()];
        o.timeout = Some(Duration::from_secs(5));
        o.args = vec!["parse".into()];
        let json = render_json(&o);
        assert!(json.contains(r#""target":"pkg:unit""#), "{json}");
        assert!(json.contains(r#""case":"parse/deep""#), "{json}");
        assert!(json.contains(r#""label":"pkg:unit/parse/deep""#), "{json}");
        assert!(json.contains(r#""labels":["slow"]"#), "{json}");
        assert!(json.contains(r#""should_fail":false"#), "{json}");
        assert!(json.contains(r#""timeout":5"#), "{json}");
        assert!(json.contains(r#""args":["parse"]"#), "{json}");

        // 事例を持たないターゲットでは `case` が無く、綴りは目標と同じ。
        let plain = render_json(&outcome(true, Some(0), None));
        assert!(plain.contains(r#""case":null"#), "{plain}");
        assert!(plain.contains(r#""label":"pkg:unit""#), "{plain}");
        assert!(plain.contains(r#""timeout":null"#), "{plain}");
    }

    #[test]
    fn a_crash_is_not_the_failure_that_should_fail_expects() {
        // `should_fail` が述べているのは「非零で終了すること」である。
        // 落ちることではない（issue #88）。
        let mut o = outcome(false, None, None);
        o.should_fail = true;
        o.signal = Some(11);
        let why = o.failure_reason().unwrap();
        assert!(why.contains("killed by signal 11 (SIGSEGV)"), "{why}");
        assert!(why.contains("not a crash"), "{why}");
        assert!(render_json(&o).contains(r#""signal":11"#));

        // `should_fail` を書いていない事例でも、シグナルはそう述べる。
        let mut plain = outcome(false, None, None);
        plain.signal = Some(6);
        assert_eq!(plain.failure_reason().unwrap(), "killed by signal 6 (SIGABRT)");

        // 番号が系によって違うものには名前を付けない。
        let mut unknown = outcome(false, None, None);
        unknown.signal = Some(7);
        assert_eq!(unknown.failure_reason().unwrap(), "killed by signal 7");
    }

    #[test]
    fn a_timeout_is_not_reported_as_a_signal() {
        // 殺したのはこちらである。プログラムの終わり方ではない。
        let mut o = outcome(false, None, None);
        o.timed_out = true;
        assert_eq!(o.failure_reason().unwrap(), "timed out and was killed");
        assert!(render_json(&o).contains(r#""signal":null"#));
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
        rerun.target_label = "pkg:a".into();
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
            transfer: None,
        };
        let (program, args) = l.command(Path::new("/tmp/unit"));
        assert_eq!(program, "qemu-riscv64");
        assert_eq!(args, vec!["-L", "/usr/riscv64-linux-gnu", "/tmp/unit"]);
        assert!(l.transfer_command(Path::new("/tmp/unit")).is_none());
    }

    #[test]
    fn a_transfer_appends_the_source_and_the_destination() {
        // パスはマニフェストに書かせず、末尾に付け足す（ADR-0008）。
        let l = Launcher {
            program: Some("ssh".into()),
            args: vec!["board.local".into()],
            transfer: Some(Transfer {
                command: vec!["scp".into(), "-q".into()],
                remote_dir: "/tmp/dowel".into(),
                host: Some("board.local".into()),
            }),
        };
        let binary = Path::new("/build/bin/unit_test");

        let (program, args) = l.transfer_command(binary).expect("a transfer was declared");
        assert_eq!(program, "scp");
        assert_eq!(args, vec!["-q", "/build/bin/unit_test", "board.local:/tmp/dowel/unit_test"]);

        // 起動側へ渡すのは対象機のパス。ローカルのパスでは対象機に存在しない。
        let (program, args) = l.command(binary);
        assert_eq!(program, "ssh");
        assert_eq!(args, vec!["board.local", "/tmp/dowel/unit_test"]);
    }

    #[test]
    fn a_transfer_without_a_host_uses_the_bare_remote_path() {
        // シリアル書き込みのように、宛先がホスト名を持たない場合。
        let l = Launcher {
            program: Some("run-on-device".into()),
            args: vec!["/dev/ttyUSB0".into()],
            transfer: Some(Transfer {
                command: vec!["flash".into()],
                remote_dir: "/lib/tests/".into(),
                host: None,
            }),
        };
        let binary = Path::new("/build/bin/unit_test");
        let (program, args) = l.transfer_command(binary).unwrap();
        assert_eq!(program, "flash");
        // `remote_dir` の末尾のスラッシュは重ねない。
        assert_eq!(args, vec!["/build/bin/unit_test", "/lib/tests/unit_test"]);
        assert_eq!(l.command(binary).1, vec!["/dev/ttyUSB0", "/lib/tests/unit_test"]);
    }
}
