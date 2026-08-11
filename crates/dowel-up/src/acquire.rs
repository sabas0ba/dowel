//! 上流からの解決と取得。
//!
//! すべての指定子はここで commit sha に落ち、以後は sha が正本になる
//! （docs/adr/0013-self-acquisition.md）。取得はソースからのビルドであり、
//! 履歴とネットワークの操作は git に、ビルドは cargo に委譲する。

use crate::prebuilt;
use crate::proc;
use crate::spec::{self, Spec};
use crate::store::{self, Home};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const DEFAULT_UPSTREAM: &str = "https://github.com/sabas0ba/dowel";

/// 上流の URL。引数 → 環境変数 → 既定の順で決める。
pub fn upstream(flag: Option<&str>) -> String {
    if let Some(u) = flag {
        return u.to_string();
    }
    if let Ok(u) = std::env::var("DOWELUP_UPSTREAM") {
        if !u.is_empty() {
            return u;
        }
    }
    DEFAULT_UPSTREAM.to_string()
}

pub struct Acquired {
    pub sha: String,
    pub already_installed: bool,
}

/// mirror を返す。無ければ clone する。返り値の bool は「作りたてか」。
/// 作りたての mirror は最新であり、続けての fetch を省ける。
fn ensure_mirror(home: &Home, url: &str) -> Result<(PathBuf, bool), String> {
    let dir = home.mirror();
    if dir.join("HEAD").is_file() {
        return Ok((dir, false));
    }
    std::fs::create_dir_all(&home.root)
        .map_err(|e| format!("cannot create {}: {e}", home.root.display()))?;
    eprintln!("cloning {url}");
    let args: [&OsStr; 4] = ["clone".as_ref(), "--mirror".as_ref(), url.as_ref(), dir.as_os_str()];
    proc::git(None, &args)?;
    Ok((dir, true))
}

/// 指定子を commit sha と、解決に使った release タグに落とす。
///
/// 手元で完結する場合（完全な sha が既に mirror にある）を除き、上流の
/// 最新の参照を取り込んでから解決する。
///
/// タグは事前ビルドの資産を探すのに要る（ADR-0036）。タグを経由しない
/// 指定子（`nightly` / `branch:` / sha）では `None` であり、そのまま
/// 「事前ビルドは無い」を意味する。
pub fn resolve(home: &Home, url: &str, spec: &Spec) -> Result<(String, Option<String>), String> {
    let (mirror, fresh) = ensure_mirror(home, url)?;
    if let Spec::Sha(h) = spec {
        // 完全な sha はそれ自身が正本であり、手元にあれば問い合わせは要らない。
        if h.len() == 40
            && proc::git_ok(Some(&mirror), &["cat-file", "-e", &format!("{h}^{{commit}}")])
        {
            return Ok((h.clone(), None));
        }
    }
    if !fresh {
        eprintln!("updating from {url}");
        proc::git(Some(&mirror), &["remote", "update", "--prune"])?;
    }
    let mut from_tag: Option<String> = None;
    let sha = match spec {
        Spec::Stable => {
            let tags = proc::git(Some(&mirror), &["tag", "--list"])?;
            let best = tags.lines().filter_map(|t| release(t).map(|v| (v, t))).max();
            let Some((_, tag)) = best else {
                return Err(format!(
                    "no release tag exists at {url} yet; use nightly, branch:<name>, or a commit hash"
                ));
            };
            from_tag = Some((*tag).to_string());
            rev_parse(&mirror, &format!("refs/tags/{tag}^{{commit}}"))
                .ok_or_else(|| format!("cannot resolve the tag {tag}"))?
        }
        Spec::Version(v) => [format!("v{v}"), v.clone()]
            .iter()
            .find_map(|t| {
                let sha = rev_parse(&mirror, &format!("refs/tags/{t}^{{commit}}"))?;
                from_tag = Some(t.clone());
                Some(sha)
            })
            .ok_or_else(|| format!("no tag v{v} or {v} exists at {url}"))?,
        Spec::Nightly => {
            rev_parse(&mirror, "HEAD").ok_or_else(|| format!("cannot resolve HEAD at {url}"))?
        }
        Spec::NightlyDate(d) => {
            let out = proc::git(
                Some(&mirror),
                &["rev-list", "-1", &format!("--before={d} 23:59:59 +0000"), "HEAD"],
            )?;
            if out.is_empty() {
                return Err(format!("no commit exists on the default branch on or before {d}"));
            }
            out
        }
        Spec::Branch(b) => rev_parse(&mirror, &format!("refs/heads/{b}^{{commit}}"))
            .ok_or_else(|| format!("no branch `{b}` exists at {url}"))?,
        Spec::Tag(t) => {
            from_tag = Some(t.clone());
            rev_parse(&mirror, &format!("refs/tags/{t}^{{commit}}"))
                .ok_or_else(|| format!("no tag `{t}` exists at {url}"))?
        }
        Spec::Sha(h) => rev_parse(&mirror, &format!("{h}^{{commit}}"))
            .ok_or_else(|| format!("no commit `{h}` exists at {url}"))?,
    };
    if sha.len() != 40 || !spec::is_hex(&sha) {
        return Err(format!("resolving `{spec}` produced `{sha}`, which is not a commit hash"));
    }
    Ok((sha, from_tag))
}

/// 解決して取得し、`versions/<sha>/` に置く。既に在れば何もしない。
///
/// 事前ビルドを先に試し、無ければソースから組む（ADR-0036）。`from_source`
/// が真なら事前ビルドは見ない。どちらを通ったかは必ず述べる——信頼の根が
/// 違うためである。ソースからのビルドは「この commit から作られた」ことを
/// 構成上示すが、事前ビルドが示せるのは「公開された一覧と同じバイト列で
/// ある」ことだけである。
pub fn install(home: &Home, url: &str, spec: &Spec, from_source: bool) -> Result<Acquired, String> {
    let (sha, tag) = resolve(home, url, spec)?;
    if home.bin(&sha).is_file() {
        // 実体は再利用するが、この指定子で解決したという記録は残す。
        // 残さないと、成功した指定子で `+<指定子>` が選べない（issue #39）。
        store::record_origin(home, &sha, &spec.to_string(), url)?;
        return Ok(Acquired { sha, already_installed: true });
    }
    let work = home.workdir(&sha);
    // 前回の失敗の残骸があっても、作り直せば正しい状態になる。
    let _ = std::fs::remove_dir_all(&work);
    if let Some(parent) = work.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }

    // 事前ビルドが在るのは release タグだけである（ADR-0036）。タグを
    // 経由しない指定子は、そのままソースへ落ちる。
    if !from_source {
        match &tag {
            Some(tag) => match prebuilt::fetch(&work, url, tag) {
                Ok(binary) => {
                    place(home, &sha, &binary)?;
                    eprintln!("installed {sha} from a release asset (verified by sha256)");
                    store::record_origin(home, &sha, &spec.to_string(), url)?;
                    let _ = std::fs::remove_dir_all(&work);
                    return Ok(Acquired { sha, already_installed: false });
                }
                // 資産が無いのは誤りではない。新しい三つ組は、資産が出来る
                // 前からソースで動く。落ちた跡は消す——残すと、続く
                // checkout の clone 先が空でなくなる。
                Err(prebuilt::Unavailable(why)) => {
                    let _ = std::fs::remove_dir_all(&work);
                    eprintln!("no usable release asset ({why}); building from source");
                }
            },
            None => eprintln!("`{spec}` does not name a release; building from source"),
        }
    }

    eprintln!("checking out {sha}");
    let mirror = home.mirror();
    let args: [&OsStr; 4] =
        ["clone".as_ref(), "--no-checkout".as_ref(), mirror.as_os_str(), work.as_os_str()];
    proc::git(None, &args)?;
    proc::git(Some(&work), &["checkout", "--quiet", "--detach", &sha])?;
    // ロックがあれば従う（再現性）。無い版はそのままビルドする。
    let locked = work.join("Cargo.lock").is_file();
    eprintln!("building {sha} (cargo build --release{})", if locked { " --locked" } else { "" });
    proc::cargo_build(&work, locked)?;
    let built = work.join("target").join("release").join("dowel");
    if !built.is_file() {
        return Err(format!(
            "the build did not produce target/release/dowel under {}",
            work.display()
        ));
    }
    place(home, &sha, &built)?;
    eprintln!("installed {sha} built from source");
    store::record_origin(home, &sha, &spec.to_string(), url)?;
    // 成果物を置いた後の作業木は要らない。失敗した場合は調査のために残る。
    let _ = std::fs::remove_dir_all(&work);
    Ok(Acquired { sha, already_installed: false })
}

/// 実行ファイルを `versions/<sha>/` へ置く。実行権は写しで保たれる。
fn place(home: &Home, sha: &str, from: &Path) -> Result<(), String> {
    let bin = home.bin(sha);
    if let Some(parent) = bin.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::copy(from, &bin)
        .map_err(|e| format!("cannot copy the binary into {}: {e}", bin.display()))?;
    Ok(())
}

/// 解決できない参照は Err ではなく None。指定子ごとの言い換えは呼び出し側が持つ。
fn rev_parse(mirror: &Path, what: &str) -> Option<String> {
    proc::git(Some(mirror), &["rev-parse", "--verify", "--quiet", what])
        .ok()
        .filter(|s| !s.is_empty())
}

/// release タグの判定。`vX.Y.Z` または `X.Y.Z` のみを release とみなす。
/// pre-release（`-rc1` 等）を `stable` に混ぜないための制約でもある。
fn release(tag: &str) -> Option<(u64, u64, u64)> {
    let body = tag.strip_prefix('v').unwrap_or(tag);
    let mut it = body.split('.');
    let (a, b, c) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    Some((num(a)?, num(b)?, num(c)?))
}

fn num(text: &str) -> Option<u64> {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_three_part_numeric_tags_are_releases() {
        assert_eq!(release("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(release("0.10.2"), Some((0, 10, 2)));
        assert_eq!(release("v1.2"), None);
        assert_eq!(release("v1.2.3-rc1"), None);
        assert_eq!(release("nightly"), None);
    }
}
