//! 実行の下請け。
//!
//! バックエンドが共通で使うもの——失敗の表現、`PATH` の探索、そして
//! 「直前の実行で各出力を作ったコマンド」の記録——を置く。走らせ方そのものは
//! `backend` にある（[ADR-0018](../../../docs/adr/0018-backend-layer.md)）。

use crate::backend::{BuildGraph, Step};
use dowel_support::{log_debug, log_trace};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug)]
pub struct Failure {
    pub description: String,
    pub command: String,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Failure {
    /// 起動そのものに失敗した、あるいは書き出せなかった場合。
    pub fn of(description: &str, command: String, reason: String) -> Failure {
        Failure {
            description: description.to_string(),
            command,
            status: None,
            stdout: String::new(),
            stderr: reason,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{} failed", self.description)?;
        writeln!(f, "  command: {}", self.command)?;
        if let Some(c) = self.status {
            writeln!(f, "  exit status: {c}")?;
        }
        if !self.stdout.trim().is_empty() {
            writeln!(f, "--- stdout ---\n{}", self.stdout.trim_end())?;
        }
        if !self.stderr.trim().is_empty() {
            writeln!(f, "--- stderr ---\n{}", self.stderr.trim_end())?;
        }
        Ok(())
    }
}

/// `PATH` に実行可能ファイルがあるか。
///
/// 起動して確かめない。`check` の中で呼ぶため、プロセスを起こす余裕がない
/// （起動予算は 10ms、docs/20-architecture.md 5.4）。区切りを含む名前は
/// パスとして扱う。
pub fn program_exists(name: &str) -> bool {
    resolve(name).is_some()
}

/// 起動される実体の道。
///
/// 名前だけでは同一性を採れない。`PATH` の前の方に別の `cc` が現れれば、
/// 同じ名前で別のものが走る（[ADR-0055](../../../docs/adr/0055-tool-identity-in-freshness.md)）。
pub fn resolve(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return is_executable(p).then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else { return false };
    if !m.is_file() {
        return false;
    }
    // 実行ビットは Unix でのみ意味を持つ。他の環境では存在だけを見る。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// `<prog> --version` が成功するか。生成器を起動できるかの判定に使う。
pub fn responds_to_version(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 進捗の1行。
///
/// ログではなく**出力**である。段階を追えることは走らせている間に見えなければ
/// 意味がなく、ログの既定（`warn`）では見えない
/// （[ADR-0057](../../../docs/adr/0057-progress-is-shown-while-it-runs.md)）。
///
/// stderr へ出す。stdout は機械向けの出力に取ってある（docs/60-cli.md）ので、
/// `dowel graph --format=dot | dot` の途中に進捗が混ざることはない。黙るのは
/// `--log-level=off` のときだけ——利用者が黙らせる術はそれ1つである。
pub fn progress(line: &str) {
    if dowel_support::log::level() == dowel_support::log::Level::Off {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{line}");
}

/// 生成器（ninja / make）を起動し、その進捗を**届いた端から**見せる。
///
/// 溜めてから出していた頃は、1.3 秒のビルドの 11 行が最後の 19ms に固まって
/// 現れた。走っている間は何も出ないので、大きなビルドでは止まって見える
/// （ADR-0057）。
///
/// 子の stdout と stderr は別の糸で読む。1つの糸で順に読むと、片方の管が
/// 一杯になったときにもう片方が進まず、子ごと止まる。
pub fn drive(program: &str, args: &[String], build_dir: &Path) -> Result<(), Failure> {
    let shown = format!("{program} {}", args.join(" "));
    let mut cmd = Command::new(program);
    cmd.args(args);
    // 生成器をビルドディレクトリで起動する。`.ninja_log` のような作業ファイルは
    // 作業ディレクトリに書かれるため、指定しないと利用者のプロジェクトルートに
    // 散らかる。生成したファイル内のパスは全て絶対であり、作業ディレクトリを
    // 変えても解決結果は変わらない。
    cmd.current_dir(build_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    dowel_support::log_info!("{shown}");
    let start = |e: std::io::Error| {
        Failure::of(
            &format!("starting {program}"),
            shown.clone(),
            format!("{e}. `--backend=direct` runs without an external generator"),
        )
    };
    let mut child = cmd.spawn().map_err(start)?;
    let out = child.stdout.take().expect("stdout is piped");
    let err = child.stderr.take().expect("stderr is piped");

    let collecting = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(err).read_to_string(&mut text);
        text
    });
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        progress(&line);
    }
    let status = child.wait().map_err(start)?;
    let stderr = collecting.join().unwrap_or_default();

    if status.success() {
        return Ok(());
    }
    Err(Failure {
        description: program.to_string(),
        command: shown,
        // 子の stdout は既に流し終えている。失敗の報告に重ねて入れると、
        // 同じ行が画面に2度並ぶ。
        stdout: String::new(),
        status: status.code(),
        stderr,
    })
}

/// 直前の実行で各出力を作ったコマンド。
///
/// 更新時刻の比較だけでは、フラグを変えただけの再ビルドを取りこぼす。
/// ソースもヘッダも変わっておらず、時刻も動かないためである。結果は
/// 「古いフラグで作られた成果物」であり、しかも成功として報告される。
/// ninja は同じ問題を `.ninja_log` のコマンドハッシュで解いており、
/// direct 実行にも同じものが要る。
///
/// 記録するのはコマンド列の指紋であって本文ではない。本文は引用や区切り記号を
/// 含み、行指向の記録に載せると escape の仕様を持つことになる。
/// 「変わったかどうか」しか要らないため、指紋で足りる。
#[derive(Default)]
pub struct CommandLog {
    by_output: std::collections::BTreeMap<PathBuf, u64>,
}

const COMMAND_LOG: &str = "direct-log.tsv";

/// 記録と突き合わせた結果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recorded {
    /// この出力を作ったという記録が無い
    Absent,
    /// 記録は在るが、別のコマンドである
    Different,
    /// 記録どおりのコマンドである
    Same,
}

impl CommandLog {
    /// グラフが指示するコマンド。「こうなるべき」の側。
    pub fn of(g: &BuildGraph) -> CommandLog {
        let mut log = CommandLog::default();
        for s in &g.steps {
            if let Some(out) = s.outputs.first() {
                log.by_output.insert(out.clone(), fingerprint(&s.command_line()));
            }
        }
        log
    }

    /// 前回の記録。無ければ空。空は「全て作り直す」という保守的な側に倒れる。
    pub fn load(build_dir: &Path) -> CommandLog {
        let mut log = CommandLog::default();
        let Ok(text) = std::fs::read_to_string(build_dir.join(COMMAND_LOG)) else {
            log_trace!("no command log yet; every step counts as changed");
            return log;
        };
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if let Some((fp, out)) = line.split_once('\t') {
                if let Ok(fp) = fp.parse::<u64>() {
                    log.by_output.insert(PathBuf::from(out), fp);
                }
            }
        }
        log_debug!("loaded {} recorded commands", log.by_output.len());
        log
    }

    /// 今回の記録を重ねる。同じ出力については今回が勝つ。
    pub fn absorb(&mut self, current: &CommandLog) {
        for (out, fp) in &current.by_output {
            self.by_output.insert(out.clone(), *fp);
        }
    }

    /// このステップを前回と同じコマンドで作ったか。
    ///
    /// 「記録が無い」と「記録が違う」を分ける。どちらも作り直す点は同じだが、
    /// 述べるときには別のことである——1度も組んでいないビルド木で「命令が
    /// 変わった」と言えば、初回のビルドが全部誰かの編集のせいになる。
    pub fn verdict(&self, step: &Step) -> Recorded {
        let Some(out) = step.outputs.first() else { return Recorded::Absent };
        match self.by_output.get(out) {
            None => Recorded::Absent,
            Some(fp) if *fp == fingerprint(&step.command_line()) => Recorded::Same,
            Some(_) => Recorded::Different,
        }
    }

    pub fn save(&self, build_dir: &Path) {
        if std::fs::create_dir_all(build_dir).is_err() {
            return;
        }
        let mut text = String::from("# dowel. <command fingerprint>\\t<output>\n");
        for (out, fp) in &self.by_output {
            text.push_str(&format!("{fp}\t{}\n", out.display()));
        }
        // 書けなくても実行そのものは成功している。次回が全て作り直すだけであり、
        // ここで失敗を報告すると誤解を招く。
        let _ = std::fs::write(build_dir.join(COMMAND_LOG), text);
    }
}

fn fingerprint(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// この段を走らせる理由。`None` が返らないかぎり走る。
///
/// 判定と、その報告が同じ関数を読む。`--backend=direct` は走らせる直前に
/// これを呼び、`dowel status` は走らせずに同じものを呼ぶ——判定と報告が
/// それぞれの写しを持てば、報告しない理由で走り、走らない理由を報告する
/// ようになる（[ADR-0058](../../../docs/adr/0058-a-command-a-backend-cannot-spell.md)
/// が命令の綴りで避けたのと同じずれである）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stale {
    /// この出力を作ったという記録が無い。まだ1度も組んでいない
    NeverRun,
    /// 前回この出力を作ったコマンドと違う
    CommandChanged,
    /// 出力が無い
    OutputMissing(PathBuf),
    /// depfile を宣言しているのに記録が無い
    NoDependencyRecord(PathBuf),
    /// 入力が消えている
    InputMissing(PathBuf),
    /// 入力が出力より新しい
    InputNewer(PathBuf),
    /// 道具の刻印が書き換わる（ADR-0055）。走らせる側は書かれた後に判定する
    /// ので出会わない。走らせずに述べる側だけが持つ
    ToolChanged(PathBuf),
    /// 先に走る段がこの入力を書き直す。同じく、走らせずに述べる側だけが持つ
    InputRebuilt(PathBuf),
    /// 先に走る段に待たされている。辿る道が無いときだけこちらになる
    InputRebuiltBy(String),
}

impl Stale {
    /// この理由を1行で述べる。走らせる側の trace と、述べる側の表が同じ言葉を使う。
    pub fn say(&self) -> String {
        match self {
            Stale::NeverRun => "no record of a previous run for this output".to_string(),
            Stale::CommandChanged => "the command changed since the last run".to_string(),
            Stale::OutputMissing(p) => format!("output missing {}", p.display()),
            Stale::NoDependencyRecord(p) => {
                format!("no dependency record ({} is missing)", p.display())
            }
            Stale::InputMissing(p) => format!("input missing {}", p.display()),
            Stale::InputNewer(p) => format!("{} is newer than the output", p.display()),
            Stale::ToolChanged(p) => format!("the tool changed ({} is rewritten)", p.display()),
            Stale::InputRebuilt(p) => format!("{} is rewritten by an earlier step", p.display()),
            Stale::InputRebuiltBy(d) => format!("`{d}` runs first"),
        }
    }

    /// この理由が指す道。表示のために相対化する側が読む。
    pub fn path(&self) -> Option<&Path> {
        match self {
            Stale::NeverRun | Stale::CommandChanged | Stale::InputRebuiltBy(_) => None,
            Stale::OutputMissing(p)
            | Stale::NoDependencyRecord(p)
            | Stale::InputMissing(p)
            | Stale::InputNewer(p)
            | Stale::ToolChanged(p)
            | Stale::InputRebuilt(p) => Some(p),
        }
    }
}

/// この段を走らせる理由。無ければ最新である。
///
/// 「なぜ再実行されたのか（されなかったのか）」は最も問い合わせの多い挙動である。
pub fn staleness(step: &Step, previous: &CommandLog) -> Option<Stale> {
    // コマンドが変わっていれば、時刻を見るまでもなく作り直す。
    match previous.verdict(step) {
        Recorded::Absent => return Some(Stale::NeverRun),
        Recorded::Different => return Some(Stale::CommandChanged),
        Recorded::Same => {}
    }

    // 出力が1つでも欠けていれば再実行する。
    let mut oldest_output: Option<std::time::SystemTime> = None;
    for out in &step.outputs {
        let Some(t) = mtime(out) else {
            return Some(Stale::OutputMissing(out.clone()));
        };
        oldest_output = Some(oldest_output.map_or(t, |cur: std::time::SystemTime| cur.min(t)));
    }
    // 出力を宣言しない段は、比べる先が無い。毎回走らせる。
    let Some(oldest_output) = oldest_output else {
        return Some(Stale::OutputMissing(PathBuf::new()));
    };

    let mut inputs: Vec<PathBuf> = step.inputs.clone();
    if let Some(d) = &step.depfile {
        // depfile が宣言されているのに無い場合、このステップのヘッダ依存は
        // 1件も分からない。情報が無い状態で「最新である」と結論すると、
        // 別の機構（かつての ninja の `deps = gcc` など）が `.d` を畳んだ
        // ツリーで、ヘッダの変更が黙って見落とされる（issue #41）。
        // 保守的に組み直し、`.d` を作り直す。
        if !d.exists() {
            return Some(Stale::NoDependencyRecord(d.clone()));
        }
        inputs.extend(read_depfile(d));
    }
    for input in &inputs {
        match mtime(input) {
            // 入力が消えているなら再実行して誤りを表に出す。
            None => return Some(Stale::InputMissing(input.clone())),
            Some(t) if t > oldest_output => return Some(Stale::InputNewer(input.clone())),
            Some(_) => {}
        }
    }
    None
}

pub fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).ok().and_then(|m| m.modified().ok())
}

/// make 形式の depfile から依存を読む。
///
/// `target: a.h b.h \` の形。行末の `\` による継続と、
/// 空白のエスケープ（`\ `）を扱う。
pub fn read_depfile(path: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let joined = text.replace("\\\n", " ").replace("\\\r\n", " ");
    let Some((_, rhs)) = joined.split_once(':') else { return Vec::new() };

    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = rhs.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                cur.push(' ');
                chars.next();
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(PathBuf::from(std::mem::take(&mut cur)));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(PathBuf::from(cur));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_continuation_lines_in_a_depfile() {
        let p = scratch().join("depfile-test.d");
        std::fs::write(&p, "a.o: src/a.c \\\n  include/a.h \\\n  include/b.h\n").unwrap();
        let deps = read_depfile(&p);
        assert_eq!(
            deps,
            vec![
                PathBuf::from("src/a.c"),
                PathBuf::from("include/a.h"),
                PathBuf::from("include/b.h")
            ]
        );
    }

    #[test]
    fn reads_paths_containing_spaces() {
        let p = scratch().join("depfile-space.d");
        std::fs::write(&p, "a.o: my\\ dir/a.h\n").unwrap();
        assert_eq!(read_depfile(&p), vec![PathBuf::from("my dir/a.h")]);
    }

    #[test]
    fn a_missing_depfile_is_empty() {
        assert!(read_depfile(Path::new("/nonexistent/x.d")).is_empty());
    }
}
