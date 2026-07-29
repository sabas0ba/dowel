//! プロセスを跨いだ評価結果の再利用（[ADR-0012](../../../docs/adr/0012-store-contents.md)）。
//!
//! 格納するのは `Evaluated` だけである。ターゲット単位の派生はプロセス内の
//! メモに留める。理由は ADR-0012 に記す。
//!
//! ## 判定
//!
//! 復元してよいのは、格納したときの本文と今回の本文が同じ場合に限る。
//! `Evaluated` の指紋は本文の指紋であるため（[`crate::query`] 冒頭）、
//! レコードの指紋と一致すれば本文が一致している。
//!
//! 一致は必要条件であって十分条件ではない。指紋は 64 ビットのハッシュであり、
//! 鍵も同様である。復元した文書は自身の `FileId` を持つため、問い合わせた
//! ファイルと異なる場合は使わない。
//!
//! ## 書けない場合
//!
//! 書き手を取得できない場合は読み込みだけを行う。ストアは高速化のための
//! ものであり、無くても結果は変わらない（`dowel_store` 冒頭）。

use crate::query::Evaluations;
use dowel_eval::codec;
use dowel_eval::Document;
use dowel_store::{Inputs, Store};
use dowel_support::{log_debug, log_trace, FileId};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// 入力の記録を置くファイル名。ストアのディレクトリ内に置く。
const INPUTS: &str = "inputs";

/// 種別ごとの前置。鍵は `FileId` だけでは決まらない。
///
/// 格納する対象が増えた場合、同じ `FileId` に対して別の鍵が要る。
const KIND_EVALUATED: u64 = 1;

/// 値ログへ書くレコードの上限。1レコードの長さは `u32` で表す。
const MAX_VALUE: usize = u32::MAX as usize;

/// マニフェストの本文と同じ耐久度。編集のたびに変わる側である。
const DURABILITY_LOW: u8 = dowel_query::Durability::Low as u8;

/// 評価結果のストア。
pub struct Cache {
    root: PathBuf,
    /// 読み込み側。今回の実行の間は変わらない
    store: Store,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// 今回新しく計算した分。実行の終わりに書く
    pending: Vec<(u64, u64, Vec<u8>)>,
    restored: usize,
    /// 格納しなかった件数と、その理由の内訳
    skipped_diagnostics: usize,
}

/// 実行の終わりに書いた内容の数え上げ。試験と観測のために持つ。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Saved {
    pub written: usize,
    pub restored: usize,
    pub skipped_diagnostics: usize,
}

impl Cache {
    /// `root` の下のストアを開く。無ければ空のストアとして扱う。
    pub fn open(root: &Path) -> Cache {
        Cache {
            root: root.to_path_buf(),
            store: Store::open(root),
            state: RefCell::new(State::default()),
        }
    }

    /// ストアを持たない器。試験と、ストアを使わない経路のために持つ。
    pub fn disabled() -> Cache {
        Cache {
            root: PathBuf::new(),
            store: Store::open(Path::new("/nonexistent")),
            state: RefCell::new(State::default()),
        }
    }

    pub fn restored(&self) -> usize {
        self.state.borrow().restored
    }

    /// 入力の記録と、今回計算した評価結果を書く。
    ///
    /// 書けなくても誤りではない。次回の実行が計算し直すだけであり、
    /// 結果は変わらない。書き手を取得できない場合も同様である。
    pub fn save(&self, inputs: &Inputs) -> Saved {
        let state = self.state.borrow();
        let mut saved = Saved {
            written: 0,
            restored: state.restored,
            skipped_diagnostics: state.skipped_diagnostics,
        };
        saved.written = self.write(inputs, &state);
        // 1行にまとめるのは、どの経路を通っても同じ形で観測できるようにするため。
        // 書けなかった場合は written が 0 のまま出る。
        log_debug!(
            "store: wrote {} values, restored {}, skipped {} with diagnostics",
            saved.written,
            saved.restored,
            saved.skipped_diagnostics
        );
        saved
    }

    /// 実際に書く。書いた値の件数を返す。
    fn write(&self, inputs: &Inputs, state: &State) -> usize {
        if self.root.as_os_str().is_empty() {
            return 0;
        }
        let Ok(Some(mut writer)) = self.store.writer() else {
            log_debug!("store: not writing (no write lock)");
            return 0;
        };
        let dir = Store::dir(&self.root);
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join(INPUTS), inputs.encode());
            log_debug!("inputs: recorded {} files", inputs.len());
        }
        // 全て復元できた場合は書くものが無い。インデックスの差し替えは
        // 同期を2回伴うため、内容が変わらない実行では行わない。
        if state.pending.is_empty() {
            return 0;
        }
        let mut written = 0;
        for (key, fingerprint, bytes) in &state.pending {
            // 耐久度はマニフェストの本文と同じ Low。編集のたびに変わる側である。
            // 途中で失敗した場合はインデックスを差し替えない。追記した分は
            // どのレコードからも指されないため、次の実行から見えない。
            if writer.put(*key, *fingerprint, DURABILITY_LOW, bytes).is_err() {
                log_debug!("store: could not append a value; leaving the index alone");
                return 0;
            }
            written += 1;
        }
        if writer.commit().is_err() {
            log_debug!("store: could not commit the index");
            return 0;
        }
        written
    }
}

impl Evaluations for Cache {
    fn get(&self, file: FileId, fingerprint: u64) -> Option<Document> {
        let record = self.store.get(key_of(file))?;
        if record.fingerprint != fingerprint {
            log_trace!("store: file {} is present but the text changed", file.0);
            return None;
        }
        let bytes = self.store.value(record).ok()?;
        let doc = codec::decode_document(&bytes)?;
        // 鍵は 64 ビットのハッシュであり、衝突しないことを形式が保証していない。
        // 文書は自身の識別子を持つため、値の側で照合できる。
        if doc.file != file {
            log_debug!("store: the stored value is for file {}, not {}", doc.file.0, file.0);
            return None;
        }
        self.state.borrow_mut().restored += 1;
        log_debug!("store: restored the evaluation of file {}", file.0);
        Some(doc)
    }

    fn put(&self, file: FileId, fingerprint: u64, doc: &Document) {
        if self.root.as_os_str().is_empty() {
            return;
        }
        let key = key_of(file);
        // 既に同じ本文の記録がある場合は書かない。値ログは追記専用であり、
        // 同じ内容を書き直すとその分だけ伸びる。
        if self.store.get(key).is_some_and(|r| r.fingerprint == fingerprint) {
            return;
        }
        let bytes = codec::encode_document(doc);
        if bytes.len() > MAX_VALUE {
            log_debug!("store: file {} encodes to {} bytes; not storing", file.0, bytes.len());
            return;
        }
        log_trace!("store: queued file {} ({} bytes)", file.0, bytes.len());
        self.state.borrow_mut().pending.push((key, fingerprint, bytes));
    }

    fn skipped(&self, file: FileId) {
        log_trace!("store: file {} has diagnostics; not storing", file.0);
        self.state.borrow_mut().skipped_diagnostics += 1;
    }
}

/// ストア上の鍵。
///
/// `FileId` は正規化したパスのハッシュであり、プロセスを跨いで安定している
/// （[ADR-0009](../../../docs/adr/0009-file-identity.md)）。種別を混ぜるのは、
/// 同じファイルに対する別の対象を格納する余地を残すためである。
fn key_of(file: FileId) -> u64 {
    dowel_query::fingerprint_of(&(KIND_EVALUATED, file.0))
}

/// 前回の実行が残した入力の記録を読む。
pub fn read_inputs(root: &Path) -> Inputs {
    let path = Store::dir(root).join(INPUTS);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let inputs = Inputs::decode(&text);
            log_debug!("inputs: read {} records from {}", inputs.len(), path.display());
            inputs
        }
        Err(_) => Inputs::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dowel_eval::Document;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/persist-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("cannot create the scratch directory");
        dir
    }

    /// 評価を経ずに文書を作る。格納と復元の経路だけを見たいため、
    /// 中身は識別子を運ぶだけでよい。
    fn doc(file: FileId) -> Document {
        Document { file, tables: Vec::new(), cfg_refs: Vec::new() }
    }

    #[test]
    fn a_stored_document_comes_back_in_the_next_process() {
        let root = scratch("roundtrip");
        let file = FileId(7);
        {
            let cache = Cache::open(&root);
            cache.put(file, 0xaa, &doc(file));
            assert_eq!(cache.save(&Inputs::new()).written, 1);
        }
        // 別の `Cache` は別のプロセスに相当する。メモリ上の状態は引き継がない。
        let cache = Cache::open(&root);
        assert_eq!(cache.get(file, 0xaa).map(|d| d.file), Some(file));
        assert_eq!(cache.restored(), 1);
    }

    #[test]
    fn a_different_text_fingerprint_is_not_restored() {
        let root = scratch("changed");
        let file = FileId(7);
        {
            let cache = Cache::open(&root);
            cache.put(file, 0xaa, &doc(file));
            cache.save(&Inputs::new());
        }
        let cache = Cache::open(&root);
        assert!(cache.get(file, 0xbb).is_none(), "the text changed; the value must not be reused");
        assert_eq!(cache.restored(), 0);
    }

    #[test]
    fn a_value_belonging_to_another_file_is_not_restored() {
        // 鍵の衝突を値の側で検出する。鍵はハッシュであり、形式は衝突しないことを
        // 保証していない。手で作れないため、鍵と中身を食い違わせて書く。
        let root = scratch("collision");
        let asked = FileId(7);
        let stored = FileId(9);
        {
            let store = Store::open(&root);
            let mut w = store.writer().unwrap().expect("no other writer exists");
            w.put(key_of(asked), 0xaa, DURABILITY_LOW, &codec::encode_document(&doc(stored)))
                .unwrap();
            w.commit().unwrap();
        }
        let cache = Cache::open(&root);
        assert!(cache.get(asked, 0xaa).is_none(), "the value is for another file");
        assert_eq!(cache.restored(), 0);
    }

    #[test]
    fn an_unreadable_value_is_treated_as_absent() {
        // 形式が合わない場合、読めたところまでを使わない（`codec` 冒頭）。
        let root = scratch("corrupt");
        let file = FileId(7);
        {
            let store = Store::open(&root);
            let mut w = store.writer().unwrap().unwrap();
            w.put(key_of(file), 0xaa, DURABILITY_LOW, b"not a document").unwrap();
            w.commit().unwrap();
        }
        assert!(Cache::open(&root).get(file, 0xaa).is_none());
    }

    #[test]
    fn a_value_already_in_the_store_is_not_written_again() {
        // 値ログは追記専用である。同じ内容を書き直すとその分だけ伸びる。
        let root = scratch("no-rewrite");
        let file = FileId(7);
        {
            let cache = Cache::open(&root);
            cache.put(file, 0xaa, &doc(file));
            cache.save(&Inputs::new());
        }
        let before = std::fs::metadata(Store::dir(&root).join("values")).unwrap().len();
        let cache = Cache::open(&root);
        cache.put(file, 0xaa, &doc(file));
        assert_eq!(cache.save(&Inputs::new()).written, 0);
        let after = std::fs::metadata(Store::dir(&root).join("values")).unwrap().len();
        assert_eq!(before, after, "the value log grew without new content");
    }

    #[test]
    fn a_cache_without_a_store_neither_restores_nor_writes() {
        let cache = Cache::disabled();
        cache.put(FileId(7), 0xaa, &doc(FileId(7)));
        assert!(cache.get(FileId(7), 0xaa).is_none());
        assert_eq!(cache.save(&Inputs::new()), Saved::default());
    }

    #[test]
    fn keys_differ_between_files() {
        assert_ne!(key_of(FileId(1)), key_of(FileId(2)));
    }
}
