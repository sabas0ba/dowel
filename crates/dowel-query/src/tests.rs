//! エンジンの性質を、再計算の回数で確かめる。
//!
//! 「値が正しい」だけでは増分の意味がない。**何を計算しなかったか**が要点であり、
//! そこは数え上げでしか観測できない。

use super::*;
use std::cell::Cell;
use std::rc::Rc;

/// 試験用のキー。実際の利用側も同じ形の列挙で持つ。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Key {
    /// 入力: ファイルの中身
    Text(&'static str),
    /// 導出: 中身の長さ
    Len(&'static str),
    /// 導出: 長さが偶数か（射影。長さが変わっても偶奇が同じなら値は変わらない）
    Even(&'static str),
    /// 導出: 2つの偶奇の組み合わせ
    Both,
}

fn set(db: &Db<Key>, name: &'static str, text: &str) {
    db.set_input(Key::Text(name), text.to_string(), fingerprint_str(text), Durability::Low);
}

/// 各クエリが「何回計算されたか」を数える。
#[derive(Default)]
struct Counts {
    len: Cell<usize>,
    even: Cell<usize>,
    both: Cell<usize>,
}

fn len_query(
    db: &Db<Key>,
    counts: &Rc<Counts>,
    name: &'static str,
) -> Result<Arc<usize>, Cancelled> {
    let c = counts.clone();
    db.query(Key::Len(name), move |db| {
        c.len.set(c.len.get() + 1);
        let text = db.input::<String>(Key::Text(name))?.expect("the input is set");
        let n = text.len();
        Ok((n, n as Fingerprint))
    })
}

fn even_query(
    db: &Db<Key>,
    counts: &Rc<Counts>,
    name: &'static str,
) -> Result<Arc<bool>, Cancelled> {
    let c = counts.clone();
    let c2 = counts.clone();
    db.query(Key::Even(name), move |db| {
        c.even.set(c.even.get() + 1);
        let n = len_query(db, &c2, name)?;
        let even = *n % 2 == 0;
        Ok((even, even as Fingerprint))
    })
}

fn both_query(db: &Db<Key>, counts: &Rc<Counts>) -> Result<Arc<String>, Cancelled> {
    let c = counts.clone();
    let c2 = counts.clone();
    db.query(Key::Both, move |db| {
        c.both.set(c.both.get() + 1);
        let a = even_query(db, &c2, "a")?;
        let b = even_query(db, &c2, "b")?;
        let s = format!("{a}/{b}");
        Ok((s.clone(), fingerprint_str(&s)))
    })
}

#[test]
fn a_second_query_in_the_same_revision_is_a_hit() {
    let db = Db::new();
    let counts = Rc::new(Counts::default());
    set(&db, "a", "xxxx");

    assert_eq!(*len_query(&db, &counts, "a").unwrap(), 4);
    assert_eq!(*len_query(&db, &counts, "a").unwrap(), 4);
    assert_eq!(counts.len.get(), 1, "the query ran twice in one revision");
    assert_eq!(db.stats().hit, 1);
}

#[test]
fn writing_the_same_content_does_not_bump_the_revision() {
    let db = Db::new();
    set(&db, "a", "xxxx");
    let rev = db.revision();
    // 同じ内容での保存は何も無効化しない。
    set(&db, "a", "xxxx");
    assert_eq!(db.revision(), rev);
}

#[test]
fn changing_an_input_recomputes_what_depends_on_it() {
    let db = Db::new();
    let counts = Rc::new(Counts::default());
    set(&db, "a", "xxxx");
    assert_eq!(*len_query(&db, &counts, "a").unwrap(), 4);

    set(&db, "a", "xxxxxx");
    assert_eq!(*len_query(&db, &counts, "a").unwrap(), 6);
    assert_eq!(counts.len.get(), 2);
}

#[test]
fn early_cutoff_stops_the_change_from_propagating() {
    let db = Db::new();
    let counts = Rc::new(Counts::default());
    set(&db, "a", "xxxx");
    set(&db, "b", "yyyy");
    assert_eq!(*both_query(&db, &counts).unwrap(), "true/true");
    assert_eq!((counts.len.get(), counts.even.get(), counts.both.get()), (2, 2, 1));

    // 長さは 4 -> 6 に変わるが、偶奇は変わらない。
    // `Len` は再計算され、`Even` も再計算されるが値が同じなので、
    // その先の `Both` は再計算されない。これが early cutoff である。
    set(&db, "a", "xxxxxx");
    assert_eq!(*both_query(&db, &counts).unwrap(), "true/true");
    assert_eq!(counts.len.get(), 3, "Len should have been recomputed");
    assert_eq!(counts.even.get(), 3, "Even should have been recomputed");
    assert_eq!(counts.both.get(), 1, "Both must NOT have been recomputed");
    assert!(db.stats().cut_off >= 1, "{:?}", db.stats());
}

#[test]
fn a_real_change_does_propagate() {
    let db = Db::new();
    let counts = Rc::new(Counts::default());
    set(&db, "a", "xxxx");
    set(&db, "b", "yyyy");
    assert_eq!(*both_query(&db, &counts).unwrap(), "true/true");

    // 偶奇が変わるので、今度は先まで伝わる。
    set(&db, "a", "xxxxx");
    assert_eq!(*both_query(&db, &counts).unwrap(), "false/true");
    assert_eq!(counts.both.get(), 2);
}

#[test]
fn an_untouched_branch_is_not_recomputed() {
    let db = Db::new();
    let counts = Rc::new(Counts::default());
    set(&db, "a", "xxxx");
    set(&db, "b", "yyyy");
    both_query(&db, &counts).unwrap();
    let before = counts.len.get();

    // `a` だけ触る。`b` 側は触れられない。
    set(&db, "a", "xxxxx");
    both_query(&db, &counts).unwrap();
    assert_eq!(counts.len.get(), before + 1, "the untouched branch was recomputed");
}

#[test]
fn a_durable_memo_skips_walking_its_dependencies() {
    let db: Db<Key> = Db::new();
    // 高い耐久度の入力にだけ依存するクエリ。
    db.set_input(Key::Text("tc"), "clang-19".to_string(), 1, Durability::High);
    let ran = Rc::new(Cell::new(0usize));
    let r = ran.clone();
    let q = |db: &Db<Key>| {
        let r = r.clone();
        db.query(Key::Len("tc"), move |db| {
            r.set(r.get() + 1);
            let t = db.input::<String>(Key::Text("tc"))?.unwrap();
            Ok((t.len(), t.len() as Fingerprint))
        })
    };
    q(&db).unwrap();
    assert_eq!(ran.get(), 1);

    // 不安定な入力が変わっても、安定層のメモは依存の走査すら省ける。
    db.reset_stats();
    db.set_input(Key::Text("manifest"), "x".to_string(), 2, Durability::Low);
    q(&db).unwrap();
    assert_eq!(ran.get(), 1, "the durable query was recomputed");
    assert_eq!(db.stats().skipped, 1, "{:?}", db.stats());
    assert_eq!(db.stats().verified, 0, "dependencies should not have been walked");

    // 高い耐久度の入力が変われば当然やり直す。
    db.set_input(Key::Text("tc"), "gcc-14".to_string(), 3, Durability::High);
    q(&db).unwrap();
    assert_eq!(ran.get(), 2);
}

#[test]
fn cancellation_stops_at_the_query_boundary() {
    let db = Db::new();
    let counts = Rc::new(Counts::default());
    set(&db, "a", "xxxx");
    set(&db, "b", "yyyy");

    db.cancel();
    assert_eq!(both_query(&db, &counts), Err(Cancelled));
    assert_eq!(counts.both.get(), 0, "nothing should have been computed");

    // 打ち切りを解けば、そのまま続きから使える。
    db.uncancel();
    assert_eq!(*both_query(&db, &counts).unwrap(), "true/true");
}

#[test]
fn cancelling_midway_leaves_the_engine_usable() {
    let db: Db<Key> = Db::new();
    set(&db, "a", "xxxx");
    let flag = db.cancellation_flag();

    // 計算の途中で打ち切られる場合。
    let r = db.query(Key::Len("a"), move |db| {
        flag.store(true, Ordering::Relaxed);
        db.check_cancelled()?;
        let t = db.input::<String>(Key::Text("a"))?.unwrap();
        Ok((t.len(), t.len() as Fingerprint))
    });
    assert_eq!(r, Err(Cancelled));

    // フレームが積みっぱなしになっていないこと。解除すれば普通に計算できる。
    db.uncancel();
    let counts = Rc::new(Counts::default());
    assert_eq!(*len_query(&db, &counts, "a").unwrap(), 4);
}

#[test]
fn reading_a_key_that_was_never_set_yields_none() {
    let db: Db<Key> = Db::new();
    assert!(db.input::<String>(Key::Text("missing")).unwrap().is_none());
}

#[test]
fn dependencies_are_recorded_without_duplicates() {
    let db = Db::new();
    set(&db, "a", "xxxx");
    let ran = Rc::new(Cell::new(0usize));
    let r = ran.clone();
    // 同じ入力を2度読んでも依存は1つ。
    db.query(Key::Len("a"), move |db| {
        r.set(r.get() + 1);
        let a = db.input::<String>(Key::Text("a"))?.unwrap();
        let b = db.input::<String>(Key::Text("a"))?.unwrap();
        Ok((a.len() + b.len(), 0))
    })
    .unwrap();

    set(&db, "a", "yyyy");
    // 指紋が同じ（0 固定）なので値は変わらない扱いだが、再計算はされる。
    db.query(Key::Len("a"), {
        let r = ran.clone();
        move |db| {
            r.set(r.get() + 1);
            let a = db.input::<String>(Key::Text("a"))?.unwrap();
            Ok((a.len() * 2, 0))
        }
    })
    .unwrap();
    assert_eq!(ran.get(), 2);
}

#[test]
fn stats_distinguish_the_four_outcomes() {
    let db = Db::new();
    let counts = Rc::new(Counts::default());
    set(&db, "a", "xxxx");
    set(&db, "b", "yyyy");
    both_query(&db, &counts).unwrap();
    // 初回は全て「計算して変わった」。
    assert_eq!(db.stats().computed, 5, "{:?}", db.stats());

    db.reset_stats();
    both_query(&db, &counts).unwrap();
    // 同じ版なので全てヒット。
    assert_eq!(db.stats(), Stats { hit: 1, ..Default::default() });

    db.reset_stats();
    set(&db, "a", "xxxxxx");
    both_query(&db, &counts).unwrap();
    let s = db.stats();
    assert!(s.computed >= 1, "{s:?}");
    assert!(s.cut_off >= 1, "{s:?}");
}
