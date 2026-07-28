//! ストアの性質を確かめる。
//!
//! 中心にあるのは「任意の時点で落ちても壊れない」であり、
//! 通常の書き込みと読み出しだけでは検査にならない。壊した状態から
//! 開き直す場合を明示的に作る。

use super::input::{Change, InputKey, Inputs};
use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/store-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("cannot create the scratch directory");
    dir
}

#[test]
fn a_missing_store_reads_as_empty() {
    let s = Store::open(&scratch("missing"));
    assert!(s.is_empty());
    assert!(s.get(1).is_none());
}

#[test]
fn values_survive_a_reopen() {
    let root = scratch("roundtrip");
    {
        let s = Store::open(&root);
        let mut w = s.writer().unwrap().expect("no other writer exists");
        w.put(1, 0xaa, 0, b"first").unwrap();
        w.put(2, 0xbb, 2, b"second").unwrap();
        w.commit().unwrap();
    }
    let s = Store::open(&root);
    assert_eq!(s.len(), 2);
    let r = s.get(2).expect("key 2 was written");
    assert_eq!(r.fingerprint, 0xbb);
    assert_eq!(r.durability, 2);
    assert_eq!(s.value(r).unwrap(), b"second");
    assert_eq!(s.value(s.get(1).unwrap()).unwrap(), b"first");
}

#[test]
fn writing_the_same_key_again_replaces_the_record() {
    let root = scratch("replace");
    {
        let s = Store::open(&root);
        let mut w = s.writer().unwrap().unwrap();
        w.put(7, 1, 0, b"old").unwrap();
        w.commit().unwrap();
    }
    {
        let s = Store::open(&root);
        let mut w = s.writer().unwrap().unwrap();
        w.put(7, 2, 0, b"new value").unwrap();
        w.commit().unwrap();
    }
    let s = Store::open(&root);
    assert_eq!(s.len(), 1, "the key should appear once");
    let r = s.get(7).unwrap();
    assert_eq!(r.fingerprint, 2);
    assert_eq!(s.value(r).unwrap(), b"new value");
}

#[test]
fn a_second_writer_is_refused_while_the_first_holds_the_lock() {
    let root = scratch("lock");
    let s = Store::open(&root);
    let held = s.writer().unwrap().expect("the first writer takes the lock");

    let other = Store::open(&root);
    assert!(other.writer().unwrap().is_none(), "a second writer must not be handed out");

    // 手放せば次が取れる。取れないことは誤りではなく、書かないだけである。
    drop(held);
    assert!(other.writer().unwrap().is_some());
}

#[test]
fn an_index_pointing_past_the_value_log_is_ignored() {
    // 値ログが切り詰められた場合。インデックスは古い位置を指したままになる。
    let root = scratch("truncated-values");
    {
        let s = Store::open(&root);
        let mut w = s.writer().unwrap().unwrap();
        w.put(1, 1, 0, b"aaaa").unwrap();
        w.put(2, 2, 0, b"bbbb").unwrap();
        w.commit().unwrap();
    }
    let values = Store::dir(&root).join("values");
    // 先頭の1件分だけ残す。
    File::options().write(true).open(&values).unwrap().set_len(4).unwrap();

    let s = Store::open(&root);
    assert_eq!(s.len(), 1, "only the record that still fits should survive");
    assert_eq!(s.value(s.get(1).unwrap()).unwrap(), b"aaaa");
    assert!(s.get(2).is_none());
}

#[test]
fn a_truncated_index_drops_only_the_partial_record() {
    let root = scratch("truncated-index");
    {
        let s = Store::open(&root);
        let mut w = s.writer().unwrap().unwrap();
        w.put(1, 1, 0, b"aaaa").unwrap();
        w.put(2, 2, 0, b"bbbb").unwrap();
        w.commit().unwrap();
    }
    let index = Store::dir(&root).join("index");
    let len = std::fs::metadata(&index).unwrap().len();
    // 1件と端数。端数は解釈できないので捨てる。
    File::options().write(true).open(&index).unwrap().set_len(len - 8).unwrap();

    let s = Store::open(&root);
    assert_eq!(s.len(), 1);
}

#[test]
fn a_garbage_index_reads_as_empty_rather_than_failing() {
    let root = scratch("garbage");
    let dir = Store::dir(&root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index"), b"not an index").unwrap();

    // 値ログが無いため、どのレコードも読めない位置を指すことになる。
    let s = Store::open(&root);
    assert!(s.is_empty());
}

#[test]
fn gc_removes_other_format_versions_and_keeps_the_current_one() {
    let root = scratch("gc");
    let base = root.join(".dowel").join("cache");
    std::fs::create_dir_all(base.join("v0")).unwrap();
    std::fs::create_dir_all(base.join(FORMAT)).unwrap();
    std::fs::write(base.join(FORMAT).join("index"), b"").unwrap();

    assert_eq!(Store::gc(&root).unwrap(), 1);
    assert!(!base.join("v0").exists());
    assert!(base.join(FORMAT).exists(), "the current format must be kept");
    // 2度目は何も残っていない。
    assert_eq!(Store::gc(&root).unwrap(), 0);
}

#[test]
fn gc_on_a_project_without_a_store_does_nothing() {
    assert_eq!(Store::gc(&scratch("gc-empty")).unwrap(), 0);
}

// --- 入力の変更検出 ------------------------------------------------------

#[test]
fn an_untouched_file_is_unchanged_without_reading_it() {
    let dir = scratch("inputs-untouched");
    let path = dir.join("a.txt");
    std::fs::write(&path, b"hello").unwrap();

    let mut inputs = Inputs::new();
    inputs.record(&path, fingerprint(b"hello"));

    // 内容を読もうとしたら失敗する形にして、読まないことを確かめる。
    let change = inputs.check(&path, || panic!("the content must not be read"));
    assert_eq!(change, Change::UnchangedByStat);
}

#[test]
fn rewriting_the_same_content_is_detected_by_hashing() {
    let dir = scratch("inputs-same-content");
    let path = dir.join("a.txt");
    std::fs::write(&path, b"hello").unwrap();

    let mut inputs = Inputs::new();
    inputs.record(&path, fingerprint(b"hello"));

    // 同じ内容で書き直す。`stat` は動くが内容は変わらない。
    std::fs::write(&path, b"hello").unwrap();
    let change = inputs.check(&path, || Some(fingerprint(b"hello")));
    assert_eq!(change, Change::UnchangedByContent);
}

#[test]
fn a_real_edit_is_reported_as_changed() {
    let dir = scratch("inputs-changed");
    let path = dir.join("a.txt");
    std::fs::write(&path, b"hello").unwrap();

    let mut inputs = Inputs::new();
    inputs.record(&path, fingerprint(b"hello"));

    std::fs::write(&path, b"goodbye").unwrap();
    assert_eq!(inputs.check(&path, || Some(fingerprint(b"goodbye"))), Change::Changed);
}

#[test]
fn an_unrecorded_or_missing_file_is_unknown() {
    let dir = scratch("inputs-unknown");
    let path = dir.join("a.txt");
    std::fs::write(&path, b"hello").unwrap();

    let inputs = Inputs::new();
    assert_eq!(inputs.check(&path, || Some(0)), Change::Unknown);

    let mut inputs = Inputs::new();
    inputs.record(&path, 0);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(inputs.check(&path, || Some(0)), Change::Unknown);
}

#[test]
fn input_records_survive_encoding() {
    let dir = scratch("inputs-encode");
    let path = dir.join("a b.txt");
    std::fs::write(&path, b"hello").unwrap();

    let mut inputs = Inputs::new();
    inputs.record(&path, 12345);
    let text = inputs.encode();

    let back = Inputs::decode(&text);
    assert_eq!(back.len(), 1);
    // 空白を含むパスも通る。区切りをタブにしてあるため。
    assert_eq!(back.check(&path, || panic!("must not read")), Change::UnchangedByStat);
}

#[test]
fn broken_lines_in_the_input_record_are_skipped() {
    let text = "# header\nnot a record\n1\t2\n\n";
    assert!(Inputs::decode(text).is_empty());
}

#[test]
fn the_stat_key_changes_when_the_file_does() {
    let dir = scratch("statkey");
    let path = dir.join("a.txt");
    std::fs::write(&path, b"a").unwrap();
    let before = InputKey::of(&path).unwrap();
    std::fs::write(&path, b"aa").unwrap();
    let after = InputKey::of(&path).unwrap();
    assert_ne!(before, after);
    assert_eq!(after.size, 2);
}
