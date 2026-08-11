//! 道具について確かめた事実（[ADR-0028](../../../docs/adr/0028-probe-facts.md)）。
//!
//! ビルドの構成には、マニフェストに書かれていない入力がある——「その道具が
//! PATH に在るか」「そのコンパイラは何を名乗るか」。これらは**走った機械の
//! 状態**であり、記録されなければ再現もできず、毎回確かめ直すことになる
//! （docs/20-architecture.md 9節）。
//!
//! ## プロジェクトの外に置く
//!
//! 事実は道具のものであってプロジェクトのものではない。同じコンパイラを使う
//! 限り、答は木を跨いで同じである。`.dowel/cache/` に置くと、木の数だけ
//! 同じ問いを繰り返す——耐久性の階層で最上位に来るものが、最も揮発しやすい
//! 場所に置かれることになる。
//!
//! ```text
//! $XDG_CACHE_HOME/dowel/facts/v1/facts    1行1件。`<鍵>\t<値>`
//! ```
//!
//! ## 鍵が道具の同一性を含む
//!
//! 鍵は「問い」だけでなく**その道具が何であったか**（道すじ・大きさ・更新
//! 時刻）を含む。無効化の機構を持たないのはこのためで、道具が入れ替われば
//! 鍵が変わり、古い事実は誰にも引かれなくなる。回収は `dowel cache gc` の
//! 仕事である。
//!
//! ## 書けない場合
//!
//! 読めるが書けない（権限、同時実行）場合は黙って諦める。失うのは省略の
//! 利得だけで、答は変わらない——ストア（[`crate::Store`]）と同じ判断である。

use dowel_support::{log_debug, log_trace};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 事実の形式版。形式を変えたらこれを上げる。
pub const FORMAT: &str = "v1";

const FILE: &str = "facts";

/// 記録の上限。
///
/// 事実は小さく（1件 100 バイト前後）、数も道具の数で頭打ちになる。上限は
/// 「壊れた鍵の作り方をしたときに際限なく育たない」ための歯止めであって、
/// 運用上の制約ではない。超えたら捨てて作り直す——古い事実は確かめ直せる。
const MAX_ENTRIES: usize = 4096;

/// 道具について確かめた事実の集まり。
pub struct Facts {
    dir: PathBuf,
    entries: BTreeMap<String, String>,
    /// 書き足したものがあるか。無ければ保存で触らない
    dirty: bool,
}

impl Facts {
    /// 事実を置くディレクトリ。
    ///
    /// `XDG_CACHE_HOME`、無ければ `~/.cache`。どちらも読めない環境では
    /// 一時領域へ落とす——事実が残らないだけで、答は変わらない。
    pub fn dir() -> PathBuf {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        base.join("dowel").join("facts").join(FORMAT)
    }

    pub fn open() -> Facts {
        Facts::open_in(Facts::dir())
    }

    /// 場所を指定して開く。試験と、環境を汚さない呼び出しのために分けてある。
    pub fn open_in(dir: PathBuf) -> Facts {
        let mut entries = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(dir.join(FILE)) {
            for line in text.lines() {
                // 値に改行は入らない（採る側が1行に畳む）。壊れた行は捨てる。
                if let Some((k, v)) = line.split_once('\t') {
                    entries.insert(k.to_string(), v.to_string());
                }
            }
        }
        log_debug!("facts: {} record(s) from {}", entries.len(), dir.display());
        Facts { dir, entries, dirty: false }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        let key = &normalize(key);
        let hit = self.entries.get(key).map(String::as_str);
        match hit {
            Some(v) => log_trace!("  fact hit  {key} = {v}"),
            None => log_trace!("  fact miss {key}"),
        }
        hit
    }

    pub fn set(&mut self, key: String, value: String) {
        // 改行を含む値は行の形を壊す。採る側で畳むのが筋だが、器の側でも
        // 保つ——壊れた1行は、その後の全ての行を読めなくする。
        let key = normalize(&key);
        let value = value.replace(['\n', '\r', '\t'], " ");
        log_trace!("  fact set  {key} = {value}");
        self.entries.insert(key, value);
        self.dirty = true;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 書き出す。書けなければ黙って諦める。
    ///
    /// 一時ファイルへ書いてから `rename` で差し替える。同一ディレクトリ内の
    /// `rename` は原子的なので、途中で落ちても半端な内容が残らない。
    pub fn save(&self) {
        if !self.dirty {
            return;
        }
        if self.entries.len() > MAX_ENTRIES {
            log_debug!("facts: {} records exceed the cap; not saving", self.entries.len());
            return;
        }
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let mut text = String::new();
        for (k, v) in &self.entries {
            text.push_str(k);
            text.push('\t');
            text.push_str(v);
            text.push('\n');
        }
        // 同時に走る dowel が同じ名前を使わないよう、プロセス id を混ぜる。
        let tmp = self.dir.join(format!("{FILE}.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_err() {
            return;
        }
        if std::fs::rename(&tmp, self.dir.join(FILE)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        log_debug!("facts: saved {} record(s)", self.entries.len());
    }

    /// 古い形式版を回収する。戻り値は消したディレクトリの数。
    pub fn gc() -> std::io::Result<usize> {
        let base = Facts::dir().parent().map(Path::to_path_buf).unwrap_or_default();
        let Ok(entries) = std::fs::read_dir(&base) else { return Ok(0) };
        let mut removed = 0;
        for e in entries.flatten() {
            if e.file_name() == FORMAT {
                continue;
            }
            if e.path().is_dir() {
                log_debug!("facts: removing {}", e.path().display());
                std::fs::remove_dir_all(e.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// 鍵を1行に収まる形にする。
///
/// 行の形は `<鍵>\t<値>` であり、割るのは**最初の**タブである。鍵がタブを
/// 含むと、そこで割れて別の鍵になる——採る側は鍵を組み立てるときに区切りを
/// 欲しがるので、器の側で吸収する。呼び手がどの文字を使うかを憶えずに
/// 済むようにするための正規化である。
fn normalize(key: &str) -> String {
    key.replace(['\t', '\n', '\r'], "\u{1f}")
}

/// 道具の同一性。鍵に混ぜることで、入れ替われば事実も引かれなくなる。
///
/// 内容そのものではなく、道すじ・大きさ・更新時刻を採る。コンパイラは数十
/// メガバイトあり、毎回読むと「探索を省く」目的に反する——ビルド系が
/// ファイルの同一性に mtime を使うのと同じ判断である。
pub fn identity(program: &Path) -> String {
    let Ok(m) = std::fs::metadata(program) else {
        return format!("{}:absent", program.display());
    };
    let stamp = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}:{}:{stamp}", program.display(), m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/facts")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_fact_survives_the_process() {
        let dir = scratch("round-trip");
        let mut f = Facts::open_in(dir.clone());
        assert_eq!(f.get("cc@x"), None);
        f.set("cc@x".into(), "x86_64-unknown-linux-gnu".into());
        f.save();

        let again = Facts::open_in(dir);
        assert_eq!(again.get("cc@x"), Some("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn nothing_is_written_when_nothing_was_learned() {
        // 読むだけの実行が、事実の書き出しでディスクに触れないこと。
        let dir = scratch("clean");
        let f = Facts::open_in(dir.clone());
        f.save();
        assert!(!dir.join(FILE).exists());
    }

    #[test]
    fn a_value_that_would_break_the_line_is_folded() {
        let dir = scratch("folded");
        let mut f = Facts::open_in(dir.clone());
        f.set("k".into(), "one\ntwo\tthree".into());
        f.save();
        assert_eq!(Facts::open_in(dir).get("k"), Some("one two three"));
    }

    #[test]
    fn a_key_containing_the_separator_still_round_trips() {
        // 採る側は鍵を組み立てるときに区切りを欲しがる。行の区切りと同じ
        // 文字を使われても、別の鍵に化けてはならない。
        let dir = scratch("separator");
        let mut f = Facts::open_in(dir.clone());
        f.set("ask\tcc\t/usr/bin/cc".into(), "answer".into());
        f.set("ask\tcc".into(), "other".into());
        f.save();

        let again = Facts::open_in(dir);
        assert_eq!(again.get("ask\tcc\t/usr/bin/cc"), Some("answer"));
        assert_eq!(again.get("ask\tcc"), Some("other"));
        assert_eq!(again.len(), 2);
    }

    #[test]
    fn a_broken_line_does_not_take_the_others_with_it() {
        let dir = scratch("broken");
        std::fs::write(dir.join(FILE), "good\tvalue\nnonsense\nother\tsecond\n").unwrap();
        let f = Facts::open_in(dir);
        assert_eq!(f.get("good"), Some("value"));
        assert_eq!(f.get("other"), Some("second"));
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn the_identity_changes_when_the_tool_does() {
        let dir = scratch("identity");
        let tool = dir.join("cc");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        let before = identity(&tool);
        // 大きさが変われば同一性も変わる。時刻の分解能に依らない検査にする。
        std::fs::write(&tool, "#!/bin/sh\necho more\n").unwrap();
        assert_ne!(before, identity(&tool));
        // 無い道具にも鍵は作れる。「無い」ことも事実である。
        assert!(identity(&dir.join("nosuch")).ends_with(":absent"));
    }
}
