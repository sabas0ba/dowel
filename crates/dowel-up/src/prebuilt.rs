//! リリース資産からの取得（[ADR-0036](../../../docs/adr/0036-prebuilt-distribution.md)）。
//!
//! ソースからのビルドは Rust ツールチェーンを要求する。C と C++ の書き手に
//! 最初のビルドの前にそれを求めるのは筋が悪い。
//!
//! ここで確かめるのは**壊れていないこと**であって、すり替えられていない
//! ことではない。書庫を差し替えられる者はその隣の `.sha256` も差し替え
//! られる。捕まえられるのは途中で切れた取得、バイトを壊す串、古い鏡で
//! ある。取得の出所を証明できるのはソースからのビルドの側であり、
//! その違いは ADR に書いた。

use crate::proc;
use std::path::{Path, PathBuf};

/// 資産の名前。`<tag>` は解決に使った release タグである。
///
/// 三つ組は dowelup 自身が動いている機械のものである。ビルド対象の三つ組を
/// 近似する話（ADR-0028）とは別で、ここは「この機械で動くものが欲しい」
/// という自分自身についての問いなので、コンパイル時の定数で足りる。
pub fn asset_name(tag: &str) -> String {
    format!("dowel-{tag}-{}.tar.gz", host_triple())
}

/// この機械の三つ組。
pub fn host_triple() -> String {
    // 構成の綴りは Rust の定数と三つ組で同じである。OS だけが違う。
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

/// 資産の URL。上流の URL から導く——`--upstream` で差し替えれば、
/// 取得先も一緒に動く。
pub fn asset_url(upstream: &str, tag: &str) -> String {
    let base = upstream.trim_end_matches('/').trim_end_matches(".git");
    format!("{}/releases/download/{tag}/{}", with_scheme(base), asset_name(tag))
}

/// 図式の無い絶対パスは `file://` にする。
///
/// 上流はローカルの木でもよい（git がそう受ける）。取得の側だけが URL を
/// 要求すると、同じ `--upstream` が git には通って curl には通らない。
fn with_scheme(base: &str) -> String {
    if base.contains("://") || !base.starts_with('/') {
        return base.to_string();
    }
    format!("file://{base}")
}

/// 取れなかった理由。呼び出し側はこれを見てソースへ落ちる。
pub struct Unavailable(pub String);

/// 資産を取り、ハッシュを検め、開いて実行ファイルの場所を返す。
///
/// 検証は**開く前**に行う。開くという操作は、書庫の中身に「どこへ置くか」
/// を決めさせる操作である（ADR-0029 と同じ判断）。
pub fn fetch(work: &Path, upstream: &str, tag: &str) -> Result<PathBuf, Unavailable> {
    let url = asset_url(upstream, tag);
    let archive = work.join(asset_name(tag));
    std::fs::create_dir_all(work)
        .map_err(|e| Unavailable(format!("cannot create {}: {e}", work.display())))?;

    eprintln!("fetching {url}");
    proc::download(&url, &archive).map_err(Unavailable)?;

    let expected = {
        let sums = work.join("sha256");
        proc::download(&format!("{url}.sha256"), &sums).map_err(|e| {
            Unavailable(format!("the asset has no checksum beside it ({e}); not trusting it"))
        })?;
        let text = std::fs::read_to_string(&sums)
            .map_err(|e| Unavailable(format!("cannot read the checksum: {e}")))?;
        parse_sha256(&text)
            .ok_or_else(|| Unavailable(format!("the checksum file is not a sha256: {text:?}")))?
    };

    let actual = dowel_support::sha256::hex_of_file(&archive)
        .map_err(|e| Unavailable(format!("cannot read {}: {e}", archive.display())))?;
    if actual != expected {
        // 壊れた取得と、すり替えは、ここでは区別できない。述べられるのは
        // 「宣言と違う」ことだけである。
        return Err(Unavailable(format!(
            "the downloaded asset does not match its checksum\n  expected {expected}\n  actual   {actual}"
        )));
    }

    let unpacked = work.join("unpacked");
    std::fs::create_dir_all(&unpacked)
        .map_err(|e| Unavailable(format!("cannot create {}: {e}", unpacked.display())))?;
    proc::untar(&archive, &unpacked).map_err(Unavailable)?;

    let binary = find_binary(&unpacked).ok_or_else(|| {
        Unavailable(format!("the asset contains no `dowel` binary ({})", unpacked.display()))
    })?;
    Ok(binary)
}

/// `<64桁> <名前>` でも、64桁だけでも読む。GNU の `sha256sum` は前者を書く。
fn parse_sha256(text: &str) -> Option<String> {
    let first = text.split_whitespace().next()?;
    let ok = first.len() == 64 && first.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| first.to_ascii_lowercase())
}

/// 開いた木から実行ファイルを探す。書庫が包む段数を決め打ちにしない。
fn find_binary(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "dowel.exe" } else { "dowel" };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|f| f == name) {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_follows_the_upstream() {
        let url = asset_url("https://github.com/sabas0ba/dowel", "v1.2.3");
        assert!(url.starts_with("https://github.com/sabas0ba/dowel/releases/download/v1.2.3/"));
        assert!(url.ends_with(".tar.gz"));
        // `.git` 付きの URL と末尾の `/` を同じ場所に落とす。
        assert_eq!(asset_url("https://host/x.git", "v1"), asset_url("https://host/x/", "v1"));
        // 上流がローカルの木なら `file://` にする。git はパスをそのまま
        // 受けるので、取得の側だけが通らない形になってはならない。
        assert!(asset_url("/srv/mirror/dowel", "v1").starts_with("file:///srv/mirror/dowel/"));
    }

    #[test]
    fn a_checksum_file_is_read_in_either_shape() {
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(parse_sha256(hex), Some(hex.to_string()));
        assert_eq!(parse_sha256(&format!("{hex}  dowel.tar.gz\n")), Some(hex.to_string()));
        // 大文字でも同じ値として読む。比較は小文字で行う。
        assert_eq!(parse_sha256(&hex.to_uppercase()), Some(hex.to_string()));
        assert_eq!(parse_sha256("not a hash"), None);
        assert_eq!(parse_sha256(""), None);
    }

    #[test]
    fn the_triple_names_this_machine() {
        let t = host_triple();
        assert!(t.contains('-'), "{t}");
        assert!(!t.contains("unknown-unknown"), "{t}");
    }
}
