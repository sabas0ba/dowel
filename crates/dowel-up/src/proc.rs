//! git / cargo の起動。
//!
//! 取得・解決・ビルドは外部コマンドへ委譲する
//! （docs/adr/0013-self-acquisition.md）。ここでは呼び出しの作法を揃える。
//!
//! - 呼び出し元の環境（`GIT_DIR` 等）は遮蔽する。dowelup 自身が git
//!   リポジトリの中から起動されても、mirror の操作に混入しない
//! - stdout は取得し、進行は stderr の役割とする（docs/60-cli.md と同じ分担）

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

/// git を起動して stdout を返す。失敗はコマンド列と stderr を含めて報告する。
pub fn git<S: AsRef<OsStr>>(dir: Option<&Path>, args: &[S]) -> Result<String, String> {
    let out = git_command(dir, args).output().map_err(|e| format!("cannot start git: {e}"))?;
    if !out.status.success() {
        let shown: Vec<String> =
            args.iter().map(|a| a.as_ref().to_string_lossy().into_owned()).collect();
        return Err(format!(
            "git {} failed:\n{}",
            shown.join(" "),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 判定に使う起動。失敗を異常ではなく「否」として扱う。
pub fn git_ok<S: AsRef<OsStr>>(dir: Option<&Path>, args: &[S]) -> bool {
    git_command(dir, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn git_command<S: AsRef<OsStr>>(dir: Option<&Path>, args: &[S]) -> Command {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    cmd.args(args)
        .stdin(Stdio::null())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cmd
}

/// checkout の中で cargo build を起動する。進行は stderr へ素通しする。
///
/// 出力先は checkout 内の `target/` に固定する。環境の `CARGO_TARGET_DIR` に
/// 依らず、生成物の位置を一意にするため。
pub fn cargo_build(checkout: &Path, locked: bool) -> Result<(), String> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--release");
    if locked {
        cmd.arg("--locked");
    }
    let status = cmd
        .current_dir(checkout)
        .env("CARGO_TARGET_DIR", checkout.join("target"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("cannot start cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "cargo build --release failed; the checkout is kept at {} for inspection",
            checkout.display()
        ));
    }
    Ok(())
}

/// 取得。`curl` を試し、無ければ `wget`（ADR-0036）。
///
/// 自前で HTTP を話さないのは、ソースの取得を git に委ねたのと同じ判断で
/// ある（ADR-0013）。確立した道具の責務は奪わない。
pub fn download(url: &str, to: &Path) -> Result<(), String> {
    let out = to.to_string_lossy().into_owned();
    let attempts: [(&str, Vec<&str>); 2] = [
        // `--fail` が無いと、404 の本文を書庫として保存してしまう。
        ("curl", vec!["--fail", "--silent", "--show-error", "--location", url, "--output", &out]),
        // `--quiet` ではなく `--no-verbose`。前者は進捗と一緒に**誤りも**
        // 黙らせるので、失敗の理由が空になる（issue #145）。
        ("wget", vec!["--no-verbose", url, "-O", &out]),
    ];
    // 理由は全ての試行ぶん集める。最後のものだけ残すと、実際に失敗した
    // 道具ではなく、入っていない道具の名前が理由なしで出ることになる。
    let mut why = Vec::new();
    for (program, args) in attempts {
        match Command::new(program).args(&args).stdin(Stdio::null()).output() {
            // 道具が無いだけなら次を試す。落とすのは全てが失敗したとき。
            Err(e) => why.push(format!("cannot run {program}: {e}")),
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => {
                let _ = std::fs::remove_file(to);
                let stderr = String::from_utf8_lossy(&o.stderr);
                let stderr = stderr.trim();
                // 何も言わずに終わる道具もある。終了状態しか無いなら、
                // せめてそれを述べる——空の括弧は何も伝えない。
                why.push(if stderr.is_empty() {
                    format!("{program} failed ({})", o.status)
                } else {
                    format!("{program} failed: {stderr}")
                });
            }
        }
    }
    Err(why.join("; "))
}

/// 書庫を開く。`tar` へ委譲する。
pub fn untar(archive: &Path, into: &Path) -> Result<(), String> {
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot start tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("tar failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}
