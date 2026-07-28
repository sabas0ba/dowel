//! 入力ファイルの変更検出（docs/20-architecture.md 5.2）。
//!
//! ファイル監視を使えないため、既知の入力に対する `stat` 走査で判定する。
//! 内容ハッシュは `stat` の結果が前回と異なる場合にのみ取る。
//!
//! ## `stat` の結果だけで「変わっていない」と判断してよいか
//!
//! 判断してよい。`(mtime, size, inode, ctime)` が全て一致していて内容が
//! 異なる状態は、mtime を意図的に戻すか、mtime の粒度より短い間隔で
//! 同じ大きさに書き換えた場合にのみ起きる。前者は稀であり、後者は
//! ctime が動くため検出できる。
//!
//! 逆向き（`stat` が異なるのに内容が同じ）は頻繁に起きる。`touch`、
//! チェックアウト、同じ内容での上書き保存などである。この場合に
//! ハッシュを取って「変わっていない」と判定することが、
//! 不要な再計算を避けるうえで効く。

use std::path::Path;
use std::time::UNIX_EPOCH;

/// `stat` から取れる、内容を読まずに比較できる値。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct InputKey {
    pub mtime_ns: u128,
    pub size: u64,
    pub inode: u64,
    pub ctime_ns: u128,
}

impl InputKey {
    /// ファイルの `stat` を取る。読めない場合は `None`。
    pub fn of(path: &Path) -> Option<InputKey> {
        let m = std::fs::metadata(path).ok()?;
        let ns = |t: Option<std::time::SystemTime>| {
            t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0)
        };
        Some(InputKey {
            mtime_ns: ns(m.modified().ok()),
            size: m.len(),
            inode: inode_of(&m),
            ctime_ns: ns(created_or_none(&m)),
        })
    }
}

#[cfg(unix)]
fn inode_of(m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ino()
}

#[cfg(not(unix))]
fn inode_of(_m: &std::fs::Metadata) -> u64 {
    // inode を持たない環境では 0 とする。mtime と size だけで判定することになり、
    // 判定は粗くなるが、内容ハッシュへ落ちるだけで誤りにはならない。
    0
}

#[cfg(unix)]
fn created_or_none(m: &std::fs::Metadata) -> Option<std::time::SystemTime> {
    use std::os::unix::fs::MetadataExt;
    // Unix の ctime は「inode の最終変更時刻」であり、`created` ではない。
    // 内容を伴わない変更（改名、権限変更）でも更新されるため、変更検出の対象に含める。
    let secs = m.ctime();
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + std::time::Duration::new(secs as u64, m.ctime_nsec() as u32))
}

#[cfg(not(unix))]
fn created_or_none(m: &std::fs::Metadata) -> Option<std::time::SystemTime> {
    m.created().ok()
}

/// 既知の入力とその状態。
#[derive(Default)]
pub struct Inputs {
    entries: std::collections::BTreeMap<std::path::PathBuf, (InputKey, u64)>,
}

/// 変更検出の結果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    /// `stat` が一致した。内容は読んでいない
    UnchangedByStat,
    /// `stat` は異なるが内容の指紋が一致した
    UnchangedByContent,
    /// 内容が変わった
    Changed,
    /// 記録が無い、または読めない
    Unknown,
}

impl Inputs {
    pub fn new() -> Inputs {
        Inputs::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 入力を記録する。`fingerprint` は内容の指紋。
    pub fn record(&mut self, path: &Path, fingerprint: u64) {
        if let Some(key) = InputKey::of(path) {
            self.entries.insert(path.to_path_buf(), (key, fingerprint));
        }
    }

    /// 記録と現状を突き合わせる。
    ///
    /// `stat` が一致する場合は内容を読まない。走査の費用はこの分岐で決まる。
    pub fn check(&self, path: &Path, content: impl FnOnce() -> Option<u64>) -> Change {
        let Some((recorded, fingerprint)) = self.entries.get(path) else { return Change::Unknown };
        let Some(now) = InputKey::of(path) else { return Change::Unknown };
        if now == *recorded {
            return Change::UnchangedByStat;
        }
        match content() {
            Some(fp) if fp == *fingerprint => Change::UnchangedByContent,
            Some(_) => Change::Changed,
            None => Change::Unknown,
        }
    }

    /// 記録を1行1件で書き出す。
    ///
    /// 形式を行指向にするのは、内容が少数のパスと整数であり、
    /// 読み書きに専用の形式を用意する必要が無いためである。
    pub fn encode(&self) -> String {
        let mut out = String::from("# dowel input records\n");
        for (path, (k, fp)) in &self.entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{fp}\t{}\n",
                k.mtime_ns,
                k.size,
                k.inode,
                k.ctime_ns,
                path.display()
            ));
        }
        out
    }

    /// [`Inputs::encode`] の逆。壊れた行は読み飛ばす。
    ///
    /// 読み飛ばして構わないのは、記録が無い入力は `Unknown` として扱われ、
    /// 呼び出し側が内容を読み直すためである。
    pub fn decode(text: &str) -> Inputs {
        let mut out = Inputs::new();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(6, '\t');
            let (Some(m), Some(s), Some(i), Some(c), Some(fp), Some(path)) = (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) else {
                continue;
            };
            let (Ok(mtime_ns), Ok(size), Ok(inode), Ok(ctime_ns), Ok(fp)) =
                (m.parse(), s.parse(), i.parse(), c.parse(), fp.parse())
            else {
                continue;
            };
            out.entries.insert(
                std::path::PathBuf::from(path),
                (InputKey { mtime_ns, size, inode, ctime_ns }, fp),
            );
        }
        out
    }
}
