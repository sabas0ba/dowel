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
        writeln!(f, "{} が失敗した", self.description)?;
        writeln!(f, "  コマンド: {}", self.command)?;
        if let Some(c) = self.status {
            writeln!(f, "  終了状態: {c}")?;
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
    log_debug!("{} を書き出した", path.display());
    Ok(path)
}

pub fn run(plan: &Plan, executor: Executor, jobs: Option<usize>) -> Result<(), Failure> {
    let _phase = dowel_support::log::Phase::start("execute");
    match executor {
        Executor::Ninja => run_ninja(plan, jobs),
        Executor::Direct => run_direct(plan),
    }
}

fn run_ninja(plan: &Plan, jobs: Option<usize>) -> Result<(), Failure> {
    let file = write_ninja_file(plan).map_err(|e| Failure {
        description: "ninja ファイルの書き出し".into(),
        command: plan.build_dir.join("build.ninja").display().to_string(),
        status: None,
        stdout: String::new(),
        stderr: e.to_string(),
    })?;

    let mut cmd = Command::new("ninja");
    cmd.arg("-f").arg(&file);
    if let Some(j) = jobs {
        cmd.arg("-j").arg(j.to_string());
    }
    log_info!("ninja -f {}", file.display());
    let out = cmd.output().map_err(|e| Failure {
        description: "ninja の起動".into(),
        command: format!("ninja -f {}", file.display()),
        status: None,
        stdout: String::new(),
        stderr: format!("{e}。`--executor=direct` なら ninja なしで実行できる"),
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
    for id in plan.order() {
        let action = plan.action(id);
        if is_up_to_date(action) {
            log_trace!("最新: {}", action.description);
            skipped += 1;
            continue;
        }
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
            stderr: format!("{e}（`{}` を起動できない）", action.program),
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
        ran += 1;
    }
    log_debug!("実行 {ran} 件、最新のため省略 {skipped} 件");
    Ok(())
}

/// 出力が全ての入力より新しいか。
fn is_up_to_date(action: &Action) -> bool {
    // 出力が1つでも欠けていれば再実行する。
    let mut oldest_output: Option<SystemTime> = None;
    for out in &action.outputs {
        let Some(t) = mtime(out) else { return false };
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
            None => return false,
            Some(t) if t > oldest_output => return false,
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
    fn depfile_の継続行を読む() {
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
    fn 空白を含むパスを読む() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("depfile-space.d");
        std::fs::write(&p, "a.o: my\\ dir/a.h\n").unwrap();
        assert_eq!(read_depfile(&p), vec![PathBuf::from("my dir/a.h")]);
    }

    #[test]
    fn 存在しない_depfile_は空() {
        assert!(read_depfile(Path::new("/nonexistent/x.d")).is_empty());
    }
}
