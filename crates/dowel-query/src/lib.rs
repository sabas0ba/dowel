//! 増分クエリエンジン。
//!
//! docs/20-architecture.md 3節が求める4つを実装する。
//!
//! | 要件 | 実現 |
//! |---|---|
//! | メモ化と依存追跡 | クエリ実行中のフレームに読んだキーを積む |
//! | early cutoff | 再計算の結果が前回と同じ指紋なら依存側を無効化しない |
//! | 耐久度の階層 | 安定層は依存の走査自体を省く |
//! | キャンセル | クエリ境界で判定し、`Result` で伝播する |
//!
//! ## 計算手続きをメモに持たせる理由
//!
//! 検証（「このメモは今の版でも有効か」）は、依存を辿って各々を最新にする必要がある。
//! 依存が導出クエリの場合、それを最新にするにはその計算手続きが要る。
//! 呼び出し側から渡ってくるのは自分の手続きだけなので、**メモ自身が手続きを保持**していないと、
//! 依存が古いというだけで自分を再計算せざるを得ない。それでは導出が連なった経路で
//! early cutoff が効かなくなる。
//!
//! この制約は記述側にも及ぶ。計算手続きは `Db` からしか値を読めない（`'static`）。
//! 外の状態を捕まえた手続きは保存できない。純粋であることが型で強制される。
//!
//! ## キャンセルを `Result` にした理由
//!
//! Salsa はキャンセルをアンワインドで伝える。本システムはリリース構成で
//! `panic = "abort"` を指定しており（起動時間と単一バイナリのため）、
//! アンワインドが使えない。クエリ境界を `Result` にすることで、
//! 後から言語サーバを載せる際に呼び出し側の型を変えずに済む。
//!
//! ## 何を指紋にするか
//!
//! 指紋は**値の内容そのもの**を表さなければならない。依存側が観測できるものが
//! 指紋に含まれていないと、early cutoff が「変わっていない」と判定した後に
//! 依存側が古い派生結果を持ち続ける。スパンを含む値なら、指紋もスパンを含める。

use dowel_support::{log_debug, log_trace};
use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 値の内容を表す指紋。等しければ「同じ値」とみなす。
pub type Fingerprint = u64;

/// 入力が変わるたびに進む版。`Revision(0)` は「まだ何もしていない」。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Revision(pub u64);

/// 入力の変わりにくさ。
///
/// マニフェストは頻繁に変わり、ツールチェーンの事実はほぼ変わらない
/// （docs/20-architecture.md 3節「耐久度の階層化」）。安定層に属するメモは、
/// 不安定な入力が変わっただけなら依存の走査自体を省ける。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Durability {
    /// マニフェストなど、編集のたびに変わるもの
    Low = 0,
    /// ロック済みの依存など、たまに変わるもの
    Medium = 1,
    /// ツールチェーンの事実など、ほぼ変わらないもの
    High = 2,
}

impl Durability {
    pub const ALL: [Durability; 3] = [Durability::Low, Durability::Medium, Durability::High];
}

/// 実行が打ち切られた。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the query was cancelled")
    }
}

/// 何が起きたかの数え上げ。試験と観測のために持つ。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stats {
    /// 計算して値が変わった
    pub computed: usize,
    /// 計算したが値は同じだった（early cutoff）
    pub cut_off: usize,
    /// 依存を辿って「変わっていない」と確認した（再計算せず）
    pub verified: usize,
    /// 耐久度により依存の走査を省いた
    pub skipped: usize,
    /// 同じ版で2度目以降の問い合わせ。導出クエリのみを数える
    /// （入力の読み出しはメモの再利用ではない）
    pub hit: usize,
}

type AnyValue = Arc<dyn Any + Send + Sync>;
type ComputeFn<K> = dyn Fn(&Db<K>) -> Result<(AnyValue, Fingerprint), Cancelled>;

struct Memo<K> {
    value: Option<AnyValue>,
    fingerprint: Fingerprint,
    deps: Vec<K>,
    /// 依存の耐久度の最小値。入力自身はその入力の耐久度
    durability: Durability,
    verified_at: Revision,
    changed_at: Revision,
    /// 入力は `None`。導出クエリは計算手続きを保持する
    compute: Option<Rc<ComputeFn<K>>>,
}

struct Frame<K> {
    deps: Vec<K>,
    durability: Durability,
}

struct Inner<K> {
    revision: Revision,
    memos: BTreeMap<K, Memo<K>>,
    stack: Vec<Frame<K>>,
    /// 耐久度ごとの「その水準の入力が最後に変わった版」
    last_change: [Revision; 3],
    stats: Stats,
}

impl<K> Inner<K> {
    /// 耐久度 `d` 以上の入力が最後に変わった版。
    ///
    /// 耐久度 `d` のメモの依存は全て耐久度 `d` 以上であるため、
    /// それより下の水準の変化はそのメモに影響しない。
    fn last_change_at_or_above(&self, d: Durability) -> Revision {
        Durability::ALL
            .iter()
            .filter(|x| **x >= d)
            .map(|x| self.last_change[*x as usize])
            .max()
            .unwrap_or_default()
    }
}

/// メモ化されたクエリの集合。
///
/// 1プロセス内で使う。`RefCell` を用いるため `Sync` ではない。
/// プロセスを跨いだ再利用は永続化ストア（docs/20-architecture.md 5節）の仕事であり、
/// このメモ表がその差し替え先になる。
pub struct Db<K> {
    inner: RefCell<Inner<K>>,
    cancelled: Arc<AtomicBool>,
}

impl<K: Ord + Clone + Debug> Default for Db<K> {
    fn default() -> Db<K> {
        Db::new()
    }
}

impl<K: Ord + Clone + Debug> Db<K> {
    pub fn new() -> Db<K> {
        Db {
            inner: RefCell::new(Inner {
                revision: Revision(1),
                memos: BTreeMap::new(),
                stack: Vec::new(),
                last_change: [Revision(0); 3],
                stats: Stats::default(),
            }),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn revision(&self) -> Revision {
        self.inner.borrow().revision
    }

    pub fn stats(&self) -> Stats {
        self.inner.borrow().stats
    }

    pub fn reset_stats(&self) {
        self.inner.borrow_mut().stats = Stats::default();
    }

    /// 別スレッドから打ち切るための旗。エディタが次の打鍵で前の問い合わせを止める用途。
    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn uncancel(&self) {
        self.cancelled.store(false, Ordering::Relaxed);
    }

    pub fn check_cancelled(&self) -> Result<(), Cancelled> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// 入力を設定する。
    ///
    /// 指紋が変わらなければ版を進めない。同じ内容で書き直しただけの保存で
    /// 全体が無効化されるのを防ぐ。
    pub fn set_input<V: Any + Send + Sync>(
        &self,
        key: K,
        value: V,
        fingerprint: Fingerprint,
        durability: Durability,
    ) {
        let mut inner = self.inner.borrow_mut();
        let unchanged = inner
            .memos
            .get(&key)
            .is_some_and(|m| m.value.is_some() && m.fingerprint == fingerprint);
        if unchanged {
            log_trace!("input {key:?} unchanged, revision stays at {:?}", inner.revision);
            return;
        }
        inner.revision = Revision(inner.revision.0 + 1);
        let rev = inner.revision;
        inner.last_change[durability as usize] = rev;
        log_debug!("input {key:?} changed, revision -> {}", rev.0);
        inner.memos.insert(
            key,
            Memo {
                value: Some(Arc::new(value)),
                fingerprint,
                deps: Vec::new(),
                durability,
                verified_at: rev,
                changed_at: rev,
                compute: None,
            },
        );
    }

    /// 導出クエリ。初回に計算手続きを保存し、以後の検証にも同じものを使う。
    pub fn query<V, F>(&self, key: K, compute: F) -> Result<Arc<V>, Cancelled>
    where
        V: Any + Send + Sync,
        F: Fn(&Db<K>) -> Result<(V, Fingerprint), Cancelled> + 'static,
    {
        let boxed: Rc<ComputeFn<K>> = Rc::new(move |db| {
            let (v, fp) = compute(db)?;
            Ok((Arc::new(v) as AnyValue, fp))
        });
        {
            let mut inner = self.inner.borrow_mut();
            match inner.memos.get_mut(&key) {
                Some(m) => m.compute = Some(boxed),
                None => {
                    inner.memos.insert(
                        key.clone(),
                        Memo {
                            value: None,
                            fingerprint: 0,
                            deps: Vec::new(),
                            durability: Durability::High,
                            verified_at: Revision(0),
                            changed_at: Revision(0),
                            compute: Some(boxed),
                        },
                    );
                }
            }
        }
        let value = self.fetch(&key)?.expect("a queried key always has a memo");
        Ok(downcast(value, &key))
    }

    /// 既に設定された入力を読む。実行中のクエリの依存として記録される。
    pub fn input<V: Any + Send + Sync>(&self, key: K) -> Result<Option<Arc<V>>, Cancelled> {
        Ok(self.fetch(&key)?.map(|v| downcast(v, &key)))
    }

    /// メモを最新にして値を返す。値を持たないキーには `None`。
    fn fetch(&self, key: &K) -> Result<Option<AnyValue>, Cancelled> {
        self.check_cancelled()?;

        enum Step<K> {
            Missing,
            InputRead,
            Hit,
            SkipByDurability,
            VerifyDeps(Vec<K>),
            Recompute,
        }

        let step = {
            let inner = self.inner.borrow();
            let rev = inner.revision;
            match inner.memos.get(key) {
                None => Step::Missing,
                // 入力は `set_input` でしか変わらないため常に最新。
                // 単なる読み出しでありメモの再利用ではないので数え上げない
                Some(m) if m.compute.is_none() => Step::InputRead,
                // 値をまだ持たない（`query` で登録された直後）
                Some(m) if m.value.is_none() => Step::Recompute,
                Some(m) if m.verified_at == rev => Step::Hit,
                Some(m) if inner.last_change_at_or_above(m.durability) <= m.verified_at => {
                    Step::SkipByDurability
                }
                Some(m) => Step::VerifyDeps(m.deps.clone()),
            }
        };

        match step {
            Step::Missing => return Ok(None),
            Step::InputRead => return Ok(Some(self.finish(key))),
            Step::Hit => {
                self.inner.borrow_mut().stats.hit += 1;
                return Ok(Some(self.finish(key)));
            }
            Step::SkipByDurability => {
                let mut inner = self.inner.borrow_mut();
                let rev = inner.revision;
                inner.stats.skipped += 1;
                if let Some(m) = inner.memos.get_mut(key) {
                    m.verified_at = rev;
                }
                drop(inner);
                log_trace!("{key:?}: verified by durability");
                return Ok(Some(self.finish(key)));
            }
            Step::VerifyDeps(deps) => {
                let mut changed = false;
                for d in &deps {
                    self.fetch(d)?;
                    let inner = self.inner.borrow();
                    let dep_changed =
                        inner.memos.get(d).map(|m| m.changed_at).unwrap_or(inner.revision);
                    let verified_at =
                        inner.memos.get(key).map(|m| m.verified_at).unwrap_or_default();
                    drop(inner);
                    if dep_changed > verified_at {
                        log_trace!("{key:?}: dependency {d:?} changed");
                        changed = true;
                        break;
                    }
                }
                if !changed {
                    let mut inner = self.inner.borrow_mut();
                    let rev = inner.revision;
                    inner.stats.verified += 1;
                    if let Some(m) = inner.memos.get_mut(key) {
                        m.verified_at = rev;
                    }
                    drop(inner);
                    log_trace!("{key:?}: verified, no dependency changed");
                    return Ok(Some(self.finish(key)));
                }
            }
            Step::Recompute => {}
        }

        self.recompute(key)?;
        Ok(Some(self.finish(key)))
    }

    fn recompute(&self, key: &K) -> Result<(), Cancelled> {
        let compute = {
            let inner = self.inner.borrow();
            inner.memos.get(key).and_then(|m| m.compute.clone())
        };
        let Some(compute) = compute else {
            // 入力に計算手続きは無い。呼ばれること自体が誤り。
            return Ok(());
        };

        self.inner
            .borrow_mut()
            .stack
            .push(Frame { deps: Vec::new(), durability: Durability::High });
        let result = compute(self);
        let frame = self.inner.borrow_mut().stack.pop().expect("the query stack is unbalanced");
        let (value, fingerprint) = result?;

        let mut inner = self.inner.borrow_mut();
        let rev = inner.revision;
        let memo = inner.memos.get_mut(key).expect("the memo was created before computing");
        let had_value = memo.value.is_some();
        let same = had_value && memo.fingerprint == fingerprint;
        memo.value = Some(value);
        memo.fingerprint = fingerprint;
        memo.deps = frame.deps;
        memo.durability = frame.durability;
        memo.verified_at = rev;
        if !same {
            memo.changed_at = rev;
        }
        if same {
            inner.stats.cut_off += 1;
        } else {
            inner.stats.computed += 1;
        }
        drop(inner);
        if same {
            log_trace!("{key:?}: recomputed, value unchanged (early cutoff)");
        } else {
            log_trace!("{key:?}: recomputed, value changed");
        }
        Ok(())
    }

    /// 呼び出し元のフレームに依存として記録し、値を返す。
    fn finish(&self, key: &K) -> AnyValue {
        let mut inner = self.inner.borrow_mut();
        let (value, durability) = {
            let m = inner.memos.get(key).expect("the memo exists at this point");
            (m.value.clone().expect("a verified memo has a value"), m.durability)
        };
        if let Some(frame) = inner.stack.last_mut() {
            if !frame.deps.contains(key) {
                frame.deps.push(key.clone());
            }
            // 自分の耐久度は依存の最小値。弱い環に引きずられる。
            frame.durability = frame.durability.min(durability);
        }
        value
    }
}

fn downcast<V: Any + Send + Sync, K: Debug>(value: AnyValue, key: &K) -> Arc<V> {
    value
        .downcast::<V>()
        .unwrap_or_else(|_| panic!("query {key:?} was read at a different type than it was stored"))
}

/// 文字列の指紋。入力の指紋によく使う。
pub fn fingerprint_str(s: &str) -> Fingerprint {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// `Hash` を実装した値の指紋。
pub fn fingerprint_of<T: std::hash::Hash>(v: &T) -> Fingerprint {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests;
