//! プロセスを跨いでメモを保持するストア（docs/20-architecture.md 5節）。
//!
//! 常駐デーモンを持たない（[ADR-0002](../../../docs/adr/0002-no-daemon.md)）ため、
//! メモリ上のグラフをプロセス間で保持できない。ディスク上のストアで代替する。
//!
//! ## 構成
//!
//! ```text
//! .dowel/cache/v1/
//!   lock      単一書き手の制限に使う
//!   values    追記専用の値ログ
//!   index     固定長レコード。鍵ハッシュ・指紋・値ログ上の位置
//! ```
//!
//! インデックスは固定長レコードのため、走査に解析を要さない。値の実体は
//! 必要になるまで読まない。検証は指紋の比較で済むため、大半のレコードは
//! 値を読まずに変化の有無を判定できる。
//!
//! ## 壊れないことの担保
//!
//! 任意の時点でプロセスが落ちてもストアが壊れないことを不変条件とする
//! （docs/20-architecture.md 5.3）。担保は以下の3点による。
//!
//! - 値ログは追記専用。既存のバイト列を書き換えない
//! - インデックスは一時ファイルへ書いてから `rename` で差し替える。
//!   同一ディレクトリ内の `rename` は原子的である
//! - 読み込み時、値ログの長さを超える位置を指すレコードを捨てる。
//!   追記の途中で落ちた場合、インデックスは古いままなので通常は起きないが、
//!   逆順で書かれた場合や外部から切り詰められた場合に備える
//!
//! ## 書けない場合
//!
//! 書き手を1つに制限する。取得できない場合は読み込みのみを行い、結果を書かない。
//! 計算はプロセス内で完結するため、失うのはキャッシュの利得だけであり、
//! 結果は変わらない。

use dowel_support::{log_debug, log_trace};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub mod input;

pub use input::{InputKey, Inputs};

/// ストアの形式版。形式を変えたらこれを上げる。
///
/// 版ごとにディレクトリを分けるのは、古い版を読もうとして誤った解釈をするより、
/// 単に無いものとして扱う方が安全であるためである。古い版の回収は
/// [`Store::gc`] が行う。
pub const FORMAT: &str = "v1";

const INDEX: &str = "index";
const VALUES: &str = "values";
const LOCK: &str = "lock";

/// インデックスの1レコード。固定長。
///
/// 走査に解析を要さない形にしてある。値の実体を読まずに済ませるため、
/// 判定に要るものだけを置く。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
    /// クエリ鍵のハッシュ
    pub key: u64,
    /// 値の内容を表す指紋
    pub fingerprint: u64,
    /// 値ログ上の位置
    pub offset: u64,
    /// 値の長さ
    pub len: u32,
    /// 入力の変わりにくさ。0 が最も変わりやすい
    pub durability: u8,
}

/// 固定長レコードの大きさ。8 + 8 + 8 + 4 + 1 + 詰め物 3。
const RECORD_SIZE: usize = 32;

impl Record {
    fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.key.to_le_bytes());
        out.extend_from_slice(&self.fingerprint.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
        out.push(self.durability);
        out.extend_from_slice(&[0u8; 3]);
    }

    fn read_from(bytes: &[u8]) -> Record {
        let u64_at = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[i..i + 8]);
            u64::from_le_bytes(b)
        };
        let mut l = [0u8; 4];
        l.copy_from_slice(&bytes[24..28]);
        Record {
            key: u64_at(0),
            fingerprint: u64_at(8),
            offset: u64_at(16),
            len: u32::from_le_bytes(l),
            durability: bytes[28],
        }
    }
}

/// 読み込み専用のストア。
pub struct Store {
    dir: PathBuf,
    records: Vec<Record>,
    /// 値ログの長さ。レコードの妥当性判定に使う
    values_len: u64,
}

impl Store {
    /// `root` の下のストアを開く。無ければ空のストアとして扱う。
    ///
    /// 開けないこと自体は誤りではない。ストアは高速化のためのものであり、
    /// 無くても結果は変わらない。
    pub fn open(root: &Path) -> Store {
        let dir = Store::dir(root);
        let values_len = std::fs::metadata(dir.join(VALUES)).map(|m| m.len()).unwrap_or(0);
        let records = Store::read_index(&dir, values_len);
        log_debug!("store: {} records, {} bytes of values", records.len(), values_len);
        Store { dir, records, values_len }
    }

    pub fn dir(root: &Path) -> PathBuf {
        root.join(".dowel").join("cache").join(FORMAT)
    }

    fn read_index(dir: &Path, values_len: u64) -> Vec<Record> {
        let Ok(bytes) = std::fs::read(dir.join(INDEX)) else { return Vec::new() };
        let mut out = Vec::with_capacity(bytes.len() / RECORD_SIZE);
        let mut dropped = 0usize;
        // 端数は切り捨てる。書き込みは rename で差し替えるため通常は生じないが、
        // 外部から切り詰められた場合に備える。
        for chunk in bytes.chunks_exact(RECORD_SIZE) {
            let r = Record::read_from(chunk);
            // 値ログの外を指すレコードは読めない。捨てて先へ進む。
            if r.offset.saturating_add(r.len as u64) > values_len {
                dropped += 1;
                continue;
            }
            out.push(r);
        }
        if dropped > 0 {
            log_debug!("store: dropped {dropped} records pointing past the value log");
        }
        out.sort_by_key(|r| r.key);
        out
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// 鍵に対応するレコード。
    pub fn get(&self, key: u64) -> Option<Record> {
        self.records.binary_search_by_key(&key, |r| r.key).ok().map(|i| self.records[i])
    }

    /// 値の実体を読む。指紋の比較で済む場合は呼ばない。
    pub fn value(&self, r: Record) -> std::io::Result<Vec<u8>> {
        let mut f = File::open(self.dir.join(VALUES))?;
        f.seek(SeekFrom::Start(r.offset))?;
        let mut buf = vec![0u8; r.len as usize];
        f.read_exact(&mut buf)?;
        log_trace!("store: read {} bytes at {}", r.len, r.offset);
        Ok(buf)
    }

    /// 書き手を取得する。既に他のプロセスが持っている場合は `None`。
    ///
    /// 取得できないことは誤りではない。読み込みは続けられ、結果も変わらない。
    /// 失うのはキャッシュの利得だけである。
    pub fn writer(&self) -> std::io::Result<Option<Writer>> {
        std::fs::create_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                log_debug!("store: another process holds the write lock; not writing");
                return Ok(None);
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        let values = OpenOptions::new().create(true).append(true).open(self.dir.join(VALUES))?;
        Ok(Some(Writer {
            dir: self.dir.clone(),
            _lock: lock,
            values,
            offset: self.values_len,
            records: self.records.clone(),
        }))
    }

    /// 古い形式のストアを回収する。回収したディレクトリ数を返す。
    ///
    /// 現在の形式は残す。形式が変わると読めなくなるが、消さない限り残り続ける。
    pub fn gc(root: &Path) -> std::io::Result<usize> {
        let base = root.join(".dowel").join("cache");
        let Ok(entries) = std::fs::read_dir(&base) else { return Ok(0) };
        let mut removed = 0;
        for e in entries.flatten() {
            if e.file_name() == FORMAT {
                continue;
            }
            if e.path().is_dir() {
                log_debug!("store: removing {}", e.path().display());
                std::fs::remove_dir_all(e.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// 書き込み権を持つストア。落とすとロックが外れる。
pub struct Writer {
    dir: PathBuf,
    /// 保持している間だけロックが効く
    _lock: File,
    values: File,
    /// 次に追記する位置
    offset: u64,
    records: Vec<Record>,
}

impl Writer {
    /// 値を追記し、レコードを差し替える。
    ///
    /// 追記が先、インデックスの差し替えが後である。逆にすると、
    /// インデックスがまだ書かれていない値を指す瞬間ができる。
    pub fn put(
        &mut self,
        key: u64,
        fingerprint: u64,
        durability: u8,
        value: &[u8],
    ) -> std::io::Result<()> {
        self.values.write_all(value)?;
        let record =
            Record { key, fingerprint, offset: self.offset, len: value.len() as u32, durability };
        self.offset += value.len() as u64;
        match self.records.binary_search_by_key(&key, |r| r.key) {
            Ok(i) => self.records[i] = record,
            Err(i) => self.records.insert(i, record),
        }
        log_trace!("store: put key {key:016x} ({} bytes)", value.len());
        Ok(())
    }

    /// インデックスを書き出す。一時ファイル + `rename` で原子的に差し替える。
    ///
    /// 値ログは既に `write_all` 済みだが、ページキャッシュ上にあるだけの
    /// 可能性がある。インデックスを差し替える前に同期し、
    /// 「インデックスが指す値は必ず読める」を保つ。
    pub fn commit(&mut self) -> std::io::Result<()> {
        self.values.sync_data()?;

        let mut bytes = Vec::with_capacity(self.records.len() * RECORD_SIZE);
        for r in &self.records {
            r.write_to(&mut bytes);
        }
        let tmp = self.dir.join("index.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_data()?;
        }
        // 同一ディレクトリ内の rename は原子的。読み手は古いか新しいかのどちらかを見る。
        std::fs::rename(&tmp, self.dir.join(INDEX))?;
        log_debug!("store: committed {} records", self.records.len());
        Ok(())
    }
}

/// バイト列の指紋。
pub fn fingerprint(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests;
