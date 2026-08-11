//! git 依存の取得。
//!
//! 履歴とネットワークの操作は `git` の起動に委譲する（[ADR-0013] と同じ判断。
//! プロトコルと認証を自前で持たない）。取得はフル 40 桁の commit sha で
//! 固定されており、同じ rev の内容は常に同じである。したがって checkout は
//! `.dowel/deps/<name>-<rev 先頭12桁>/` に一度だけ作り、以後はネットワークに
//! 触れない。rev の固定がロックの役割を兼ねるため、`dowel.lock` を要さない。
//!
//! ## 失敗と原子性
//!
//! 取得は一時ディレクトリで行い、完了印（`.dowel-rev`）を書いてから
//! `rename` で所定の位置へ置く。途中で落ちた残骸は印を持たないため、
//! 次回の取得が消して作り直す。ストア（`.dowel/cache`）と違い、ここを
//! 消した後の再構築にはネットワークが要る。
//!
//! [ADR-0013]: ../../../docs/adr/0013-self-acquisition.md

use dowel_eval::Site;
use dowel_support::{log_debug, Diagnostic};
use std::path::{Path, PathBuf};
use std::process::Command;

const MARKER: &str = ".dowel-rev";

/// 取得済み checkout の置き場。`root` は根パッケージのディレクトリ。
///
/// 名前だけだと同じ依存の別 rev が衝突し、rev 全体だとパスが長い。
/// 先頭 12 桁は git 自身が既定で用いる短縮長であり、ここでも同じ判断を借りる。
pub fn checkout_dir(root: &Path, name: &str, rev: &str) -> PathBuf {
    root.join(".dowel").join("deps").join(format!("{name}-{}", &rev[..12]))
}

/// 取得済みの checkout。完了印が rev と一致する場合にのみ返す。
pub fn existing(root: &Path, name: &str, rev: &str) -> Option<PathBuf> {
    let dir = checkout_dir(root, name, rev);
    if std::fs::read_to_string(dir.join(MARKER)).is_ok_and(|s| s.trim() == rev) {
        Some(dir)
    } else {
        None
    }
}

/// checkout を確保して、そのディレクトリを返す。既に在ればネットワークに触れない。
pub fn ensure(
    root: &Path,
    name: &str,
    url: &str,
    rev: &str,
    site: Site,
) -> Result<PathBuf, Box<Diagnostic>> {
    if let Some(dir) = existing(root, name, rev) {
        log_debug!("git dependency `{name}` is already at {}", dir.display());
        return Ok(dir);
    }
    let dir = checkout_dir(root, name, rev);

    let fail = |e: String| {
        Box::new(
            Diagnostic::error(
                "unfetchable-dependency",
                format!("cannot fetch git dependency `{name}`: {e}"),
            )
            .at(site.file, site.span, "declared here")
            .note(format!("tried `{url}` at rev `{rev}`"))
            .note("fetching runs `git`; it must be on PATH and the URL reachable"),
        )
    };

    // 印の無いディレクトリは中断の残骸である。作り直す。
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| fail(format!("cannot clear {}: {e}", dir.display())))?;
    }
    let parent = dir.parent().expect("the checkout dir has a parent");
    std::fs::create_dir_all(parent)
        .map_err(|e| fail(format!("cannot create {}: {e}", parent.display())))?;
    let tmp = parent.join(format!(".tmp-{name}-{}", &rev[..12]));
    let _ = std::fs::remove_dir_all(&tmp);

    log_debug!("fetching git dependency `{name}` from {url} at {rev}");
    let fetched = (|| -> Result<(), String> {
        git(parent, &["init", "--quiet", &tmp.display().to_string()])?;
        // 指定 sha の直接取得を先に試す（浅く済む）。サーバが未参照 sha の
        // 取得を拒む場合は、全ブランチとタグの取得に切り替える。
        if git(&tmp, &["fetch", "--quiet", "--depth", "1", url, rev]).is_err() {
            git(&tmp, &["fetch", "--quiet", "--tags", url, "+refs/heads/*:refs/remotes/origin/*"])?;
        }
        git(&tmp, &["-c", "advice.detachedHead=false", "checkout", "--quiet", "--detach", rev])?;
        // 取得したものが要求どおりであることを、信じずに確かめる。
        let head = git(&tmp, &["rev-parse", "HEAD"])?;
        if head.trim() != rev {
            return Err(format!("checked out `{}`, expected `{rev}`", head.trim()));
        }
        Ok(())
    })();
    if let Err(e) = fetched {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(fail(e));
    }

    // 履歴は使わない。checkout は rev で不変であり、置き場を小さく保つ。
    let _ = std::fs::remove_dir_all(tmp.join(".git"));
    std::fs::write(tmp.join(MARKER), format!("{rev}\n"))
        .map_err(|e| fail(format!("cannot write the completion marker: {e}")))?;
    std::fs::rename(&tmp, &dir)
        .map_err(|e| fail(format!("cannot move the checkout into place: {e}")))?;
    log_debug!("fetched `{name}` into {}", dir.display());
    Ok(dir)
}

/// 書庫の置き場。名前と内容の指紋の先頭で分ける。
///
/// git の checkout と同じ形である——固定しているものが rev か内容かの違いで
/// あって、置き方の理屈は同じ。
pub fn archive_dir(root: &Path, name: &str, sha256: &str) -> PathBuf {
    root.join(".dowel").join("deps").join(format!("{name}-{}", &sha256[..12]))
}

/// 取得済みの書庫。完了印が指紋と一致する場合にのみ返す。
pub fn existing_archive(root: &Path, name: &str, sha256: &str) -> Option<PathBuf> {
    let dir = archive_dir(root, name, sha256);
    if std::fs::read_to_string(dir.join(MARKER)).is_ok_and(|s| s.trim() == sha256) {
        Some(dir)
    } else {
        None
    }
}

/// 書庫を取得して展開し、そのディレクトリを返す。
///
/// 取得と展開は外部の道具に委ねる（ADR-0013 と同じ判断）。HTTP は
/// `curl`、無ければ `wget`。展開は `tar`。**検証だけは自前で行う**——
/// 取ってきたものを検める手続きが環境によって在ったり無かったりするのは、
/// 固定の意味を薄める（ADR-0029）。
pub fn ensure_archive(
    root: &Path,
    name: &str,
    url: &str,
    sha256: &str,
    site: Site,
) -> Result<PathBuf, Box<Diagnostic>> {
    if let Some(dir) = existing_archive(root, name, sha256) {
        log_debug!("archive dependency `{name}` is already at {}", dir.display());
        return Ok(dir);
    }
    let dir = archive_dir(root, name, sha256);

    let fail = |e: String| {
        Box::new(
            Diagnostic::error(
                "unfetchable-dependency",
                format!("cannot fetch archive dependency `{name}`: {e}"),
            )
            .at(site.file, site.span, "declared here")
            .note(format!("tried `{url}`"))
            .note("fetching runs `curl` (or `wget`) and `tar`; they must be on PATH"),
        )
    };

    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| fail(format!("cannot clear {}: {e}", dir.display())))?;
    }
    let parent = dir.parent().expect("the archive dir has a parent");
    std::fs::create_dir_all(parent)
        .map_err(|e| fail(format!("cannot create {}: {e}", parent.display())))?;
    let tmp = parent.join(format!(".tmp-{name}-{}", &sha256[..12]));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| fail(format!("cannot create a work area: {e}")))?;

    log_debug!("fetching archive dependency `{name}` from {url}");
    let archive = tmp.join("archive");
    let result = (|| -> Result<(), String> {
        download(url, &archive)?;

        // 検証は展開の**前**に行う。壊れたものや別物を、展開してから
        // 気づくのでは遅い——展開は書庫の中身に道を決めさせる操作である。
        let got = dowel_support::sha256::hex_of_file(&archive)
            .map_err(|e| format!("cannot read what was downloaded: {e}"))?;
        if got != sha256 {
            return Err(format!(
                "the archive does not match its declared hash\n  expected {sha256}\n  received {got}"
            ));
        }

        let into = tmp.join("unpacked");
        std::fs::create_dir_all(&into)
            .map_err(|e| format!("cannot create {}: {e}", into.display()))?;
        unpack(&archive, &into)?;
        // 書庫はふつう `<名前>-<版>/` を1階層持つ。1つだけ在るならそれが根で
        // ある——階層を剥がす数を宣言させるより、見て決めるほうが書き手の
        // 手間が少なく、当てが外れたときも「根が1つでない」と述べられる。
        let root_dir = single_child(&into)?;
        for entry in
            std::fs::read_dir(&root_dir).map_err(|e| format!("cannot read the archive: {e}"))?
        {
            let entry = entry.map_err(|e| format!("cannot read the archive: {e}"))?;
            let to = tmp.join(entry.file_name());
            std::fs::rename(entry.path(), &to)
                .map_err(|e| format!("cannot place {}: {e}", to.display()))?;
        }
        let _ = std::fs::remove_dir_all(&into);
        let _ = std::fs::remove_file(&archive);
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(fail(e));
    }

    std::fs::write(tmp.join(MARKER), format!("{sha256}\n"))
        .map_err(|e| fail(format!("cannot write the completion marker: {e}")))?;
    std::fs::rename(&tmp, &dir)
        .map_err(|e| fail(format!("cannot move the archive into place: {e}")))?;
    log_debug!("fetched `{name}` into {}", dir.display());
    Ok(dir)
}

/// 唯一の子ディレクトリ。書庫が包んでいる1階層を剥がすために引く。
fn single_child(dir: &Path) -> Result<PathBuf, String> {
    let mut children = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|e| format!("cannot read the archive: {e}"))? {
        children.push(e.map_err(|e| format!("cannot read the archive: {e}"))?.path());
    }
    match children.as_slice() {
        [only] if only.is_dir() => Ok(only.clone()),
        // 包みが無い書庫（中身が直に並ぶ）。そのまま使う。
        _ => Ok(dir.to_path_buf()),
    }
}

/// 書庫を取ってくる。`curl` を先に試し、無ければ `wget`。
fn download(url: &str, to: &Path) -> Result<(), String> {
    let out = to.display().to_string();
    let attempts: [(&str, Vec<&str>); 2] = [
        // `--fail` が無いと、404 の本文を書庫として保存してしまう。
        ("curl", vec!["--fail", "--silent", "--show-error", "--location", url, "--output", &out]),
        ("wget", vec!["--quiet", url, "-O", &out]),
    ];
    let mut last = String::new();
    for (program, args) in attempts {
        match Command::new(program).args(&args).output() {
            // 道具が無いだけなら次を試す。落とすのは最後の1つが失敗したとき。
            Err(e) => last = format!("cannot run {program}: {e}"),
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                last = format!("{program} failed: {}", err.trim());
            }
        }
    }
    Err(last)
}

/// 書庫を展開する。形式の判別は `tar` に委ねる（`--auto-compress` の読み側）。
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let out = Command::new("tar")
        .args(["-xf", &archive.display().to_string(), "-C", &into.display().to_string()])
        .output()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("tar failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
}

/// `git` を起動して stdout を返す。非零の終了は stderr を誤りとして返す。
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("git {} failed: {}", args.first().copied().unwrap_or(""), err.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checkout_dir_is_keyed_by_name_and_rev_prefix() {
        let d = checkout_dir(Path::new("/p"), "zlib", "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(d, Path::new("/p/.dowel/deps/zlib-0123456789ab"));
    }
}
