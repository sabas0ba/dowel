//! アクションの実行。
//!
//! 2つの実行器を持つ。
//!
//! - **ninja** — 既定。実行層はそのまま既存のものを使う
//! - **direct** — 逐次実行。ninja が無い環境でも動き、
//!   何より「ninja の挙動に依存せずアクショングラフ自体が正しいか」を切り分けられる
//!
//! 直接実行は素朴な mtime 比較で最新性を判定する。ヘッダ依存は
//! コンパイラが書いた depfile を読む。ここで作った機構は将来
//! 内容アドレスによるアクションキャッシュへ置き換わる（docs/20-architecture.md 8節）。

use crate::action::Action;
use crate::plan::Plan;
use dowel_support::{log_debug, log_info, log_trace};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Executor {
    Ninja,
    Direct,
}

impl Executor {
    pub fn parse(s: &str) -> Option<Executor> {
        match s {
            "ninja" => Some(Executor::Ninja),
            "direct" => Some(Executor::Direct),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Failure {
    pub description: String,
    pub command: String,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
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

/// `ninja` が使えるか。
pub fn ninja_available() -> bool {
    Command::new("ninja")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn write_ninja_file(plan: &Plan) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(&plan.build_dir)?;
    let path = plan.build_dir.join("build.ninja");
    std::fs::write(&path, crate::ninja::generate(plan))?;
    log_debug!("wrote {}", path.display());
    Ok(path)
}

pub fn run(plan: &Plan, executor: Executor, jobs: Option<usize>) -> Result<(), Failure> {
    let _phase = dowel_support::log::Phase::start("execute");
    let result = match executor {
        Executor::Ninja => run_ninja(plan, jobs),
        Executor::Direct => run_direct(plan),
    };
    if result.is_ok() {
        // 成功した実行の全アクションを記録する。実行器を跨いでも
        // 「今ある成果物はどのコマンドで作られたか」が一貫する。
        // 途中で失敗した場合は書かない。作り直せたものまで最新扱いされると、
        // 次の実行が古い成果物を残したまま通ってしまう。
        CommandLog::of(plan).save(&plan.build_dir);
    }
    result
}

/// 直前の実行で各出力を作ったコマンド。
///
/// 更新時刻の比較だけでは、フラグを変えただけの再ビルドを取りこぼす。
/// ソースもヘッダも変わっておらず、時刻も動かないためである。結果は
/// 「古いフラグで作られた成果物」であり、しかも成功として報告される。
/// ninja は同じ問題を `.ninja_log` のコマンドハッシュで解いており、
/// direct 実行器にも同じものが要る。
///
/// 記録するのはコマンド列の指紋であって本文ではない。本文は引用や区切り記号を
/// 含み、行指向の記録に載せると escape の仕様を持つことになる。
/// 「変わったかどうか」しか要らないため、指紋で足りる。
#[derive(Default)]
struct CommandLog {
    by_output: std::collections::BTreeMap<PathBuf, u64>,
}

const COMMAND_LOG: &str = "direct-log.tsv";

impl CommandLog {
    /// 計画が指示するコマンド。「こうなるべき」の側。
    fn of(plan: &Plan) -> CommandLog {
        let mut log = CommandLog::default();
        for id in plan.order() {
            let action = plan.action(id);
            if let Some(out) = action.outputs.first() {
                log.by_output.insert(out.clone(), fingerprint(&action.command_line()));
            }
        }
        log
    }

    /// 前回の記録。無ければ空。空は「全て作り直す」という保守的な側に倒れる。
    fn load(build_dir: &Path) -> CommandLog {
        let mut log = CommandLog::default();
        let Ok(text) = std::fs::read_to_string(build_dir.join(COMMAND_LOG)) else {
            log_trace!("no command log yet; every action counts as changed");
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

    /// このアクションを前回と同じコマンドで作ったか。
    fn matches(&self, action: &Action) -> bool {
        let Some(out) = action.outputs.first() else { return false };
        self.by_output.get(out) == Some(&fingerprint(&action.command_line()))
    }

    fn save(&self, build_dir: &Path) {
        if std::fs::create_dir_all(build_dir).is_err() {
            return;
        }
        let mut text = String::from("# dowel direct executor. <command fingerprint>\\t<output>\n");
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

fn run_ninja(plan: &Plan, jobs: Option<usize>) -> Result<(), Failure> {
    let file = write_ninja_file(plan).map_err(|e| Failure {
        description: "writing the ninja file".into(),
        command: plan.build_dir.join("build.ninja").display().to_string(),
        status: None,
        stdout: String::new(),
        stderr: e.to_string(),
    })?;

    let mut cmd = Command::new("ninja");
    // ninja をビルドディレクトリで起動する。`.ninja_log` と `.ninja_deps` は
    // ninja の作業ディレクトリに書かれるため、ここを指定しないと利用者の
    // プロジェクトルートに散らかる。ninja ファイル内のパスは全て絶対であり、
    // 作業ディレクトリを変えても解決結果は変わらない。
    cmd.current_dir(&plan.build_dir);
    cmd.arg("-f").arg(&file);
    if let Some(j) = jobs {
        cmd.arg("-j").arg(j.to_string());
    }
    log_info!("ninja -f {}", file.display());
    let out = cmd.output().map_err(|e| Failure {
        description: "starting ninja".into(),
        command: format!("ninja -f {}", file.display()),
        status: None,
        stdout: String::new(),
        stderr: format!("{e}. `--executor=direct` runs without ninja"),
    })?;

    // ninja の進捗は stdout に出る。そのまま見せる。
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !stdout.trim().is_empty() {
        eprint!("{stdout}");
    }
    if out.status.success() {
        return Ok(());
    }
    Err(Failure {
        description: "ninja".into(),
        command: format!("ninja -f {}", file.display()),
        status: out.status.code(),
        stdout,
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

fn run_direct(plan: &Plan) -> Result<(), Failure> {
    let mut ran = 0usize;
    let mut skipped = 0usize;
    let previous = CommandLog::load(&plan.build_dir);
    for id in plan.order() {
        let action = plan.action(id);
        // コマンドが変わっていれば、時刻を見るまでもなく作り直す。
        if !previous.matches(action) {
            log_trace!("  stale: the command changed since the last run");
        } else if is_up_to_date(action) {
            log_trace!("up to date: {}", action.description);
            skipped += 1;
            continue;
        }
        run_action(plan, action)?;
        ran += 1;
    }
    log_debug!("ran {ran} actions, skipped {skipped} already up to date");
    Ok(())
}

fn run_action(plan: &Plan, action: &Action) -> Result<(), Failure> {
    for out in &action.outputs {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    log_info!("{}", action.description);
    log_debug!("  {}", action.command_line());

    let mut cmd = Command::new(&action.program);
    cmd.args(&action.args);
    cmd.current_dir(&plan.build_dir);
    let out = cmd.output().map_err(|e| Failure {
        description: action.description.clone(),
        command: action.command_line(),
        status: None,
        stdout: String::new(),
        stderr: format!("{e} (cannot start `{}`)", action.program),
    })?;
    if !out.status.success() {
        return Err(Failure {
            description: action.description.clone(),
            command: action.command_line(),
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        });
    }
    Ok(())
}

/// 出力が全ての入力より新しいか。
///
/// 「なぜ再実行されたのか（されなかったのか）」は最も問い合わせの多い挙動である。
/// 判断の根拠を trace に落としておく。
fn is_up_to_date(action: &Action) -> bool {
    // 出力が1つでも欠けていれば再実行する。
    let mut oldest_output: Option<SystemTime> = None;
    for out in &action.outputs {
        let Some(t) = mtime(out) else {
            log_trace!("  stale: output missing {}", out.display());
            return false;
        };
        oldest_output = Some(oldest_output.map_or(t, |cur: SystemTime| cur.min(t)));
    }
    let Some(oldest_output) = oldest_output else { return false };

    let mut inputs: Vec<PathBuf> = action.inputs.clone();
    if let Some(d) = &action.depfile {
        inputs.extend(read_depfile(d));
    }
    for input in &inputs {
        match mtime(input) {
            // 入力が消えているなら再実行して誤りを表に出す。
            None => {
                log_trace!("  stale: input missing {}", input.display());
                return false;
            }
            Some(t) if t > oldest_output => {
                log_trace!("  stale: {} is newer than the output", input.display());
                return false;
            }
            Some(_) => {}
        }
    }
    true
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok().and_then(|m| m.modified().ok())
}

/// make 形式の depfile から依存を読む。
///
/// `target: a.h b.h \` の形。行末の `\` による継続と、
/// 空白のエスケープ（`\ `）を扱う。
fn read_depfile(path: &Path) -> Vec<PathBuf> {
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

    #[test]
    fn reads_continuation_lines_in_a_depfile() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("depfile-test.d");
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
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("depfile-space.d");
        std::fs::write(&p, "a.o: my\\ dir/a.h\n").unwrap();
        assert_eq!(read_depfile(&p), vec![PathBuf::from("my dir/a.h")]);
    }

    #[test]
    fn a_missing_depfile_is_empty() {
        assert!(read_depfile(Path::new("/nonexistent/x.d")).is_empty());
    }
}
