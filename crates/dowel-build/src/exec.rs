//! 実行の下請け。
//!
//! バックエンドが共通で使うもの——失敗の表現、`PATH` の探索、そして
//! 「直前の実行で各出力を作ったコマンド」の記録——を置く。走らせ方そのものは
//! `backend` にある（[ADR-0018](../../../docs/adr/0018-backend-layer.md)）。

use crate::backend::{BuildGraph, Step};
use dowel_support::{log_debug, log_trace};
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// 生成器（ninja / make）を起動する。進捗は stdout に出るのでそのまま見せる。
pub fn drive(program: &str, args: &[String], build_dir: &Path) -> Result<(), Failure> {
    let shown = format!("{program} {}", args.join(" "));
    let mut cmd = Command::new(program);
    cmd.args(args);
    // 生成器をビルドディレクトリで起動する。`.ninja_log` のような作業ファイルは
    // 作業ディレクトリに書かれるため、指定しないと利用者のプロジェクトルートに
    // 散らかる。生成したファイル内のパスは全て絶対であり、作業ディレクトリを
    // 変えても解決結果は変わらない。
    cmd.current_dir(build_dir);
    dowel_support::log_info!("{shown}");
    let out = cmd.output().map_err(|e| {
        Failure::of(
            &format!("starting {program}"),
            shown.clone(),
            format!("{e}. `--backend=direct` runs without an external generator"),
        )
    })?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !stdout.trim().is_empty() {
        eprint!("{stdout}");
    }
    if out.status.success() {
        return Ok(());
    }
    Err(Failure {
        description: program.to_string(),
        command: shown,
        status: out.status.code(),
        stdout,
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
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
    pub fn matches(&self, step: &Step) -> bool {
        let Some(out) = step.outputs.first() else { return false };
        self.by_output.get(out) == Some(&fingerprint(&step.command_line()))
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
