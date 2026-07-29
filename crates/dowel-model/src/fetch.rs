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

/// checkout を確保して、そのディレクトリを返す。既に在ればネットワークに触れない。
pub fn ensure(
    root: &Path,
    name: &str,
    url: &str,
    rev: &str,
    site: Site,
) -> Result<PathBuf, Box<Diagnostic>> {
    let dir = checkout_dir(root, name, rev);
    if std::fs::read_to_string(dir.join(MARKER)).is_ok_and(|s| s.trim() == rev) {
        log_debug!("git dependency `{name}` is already at {}", dir.display());
        return Ok(dir);
    }

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
