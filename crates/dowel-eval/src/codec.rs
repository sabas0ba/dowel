//! 値の直列化。ストアへ格納する形式（docs/20-architecture.md 5節）。
//!
//! 形式は長さ前置のバイト列である。可読性を持たせない代わりに、
//! 解析に走査以上のことを要さない。
//!
//! ## 復元に失敗した場合
//!
//! [`decode_document`] は `Option` を返す。ストアは読めない値を「無い」ものとして
//! 扱い、その場で計算し直す。誤りとして報告する対象ではないため、
//! 失敗の理由を型で区別しない。
//!
//! この扱いにより、形式の版が合わない場合、切り詰められた場合、
//! 外部から書き換えられた場合のいずれでも、結果は変わらず速度のみを失う。
//!
//! ## `FileId` を書き換えない
//!
//! `FileId` は正規化したパスのハッシュであり、プロセスを跨いで安定している
//! （[ADR-0009](../../../docs/adr/0009-file-identity.md)）。
//! 格納した値をそのまま復元でき、識別子を振り直すための走査が生じない。
//!
//! ## 来歴の共有は保存しない
//!
//! `Prov` は `Arc` で親を共有するが、直列化では値ごとに鎖を展開する。
//! 共有を保存するには参照の同一性を識別子に写す必要があり、
//! 形式と復元の双方が複雑になる。鎖の段数は `dowel why` の表示行数と
//! 同程度であり、展開しても大きくならない。

use crate::eval::{Document, Entry, Table};
use crate::value::{
    CfgKey, Data, MatchArm, Ns, Origin, PathBase, PathValue, Pattern, Pred, Prov, Site, Type, Value,
};
use dowel_support::{FileId, Span};

/// 形式の版。形式を変えたらこれを上げる。読めない版は無いものとして扱う。
const VERSION: u8 = 1;

pub fn encode_document(doc: &Document) -> Vec<u8> {
    let mut w = W(Vec::new());
    w.0.push(VERSION);
    w.u64(doc.file.0);
    w.len(doc.tables.len());
    for t in &doc.tables {
        w.strs(&t.path);
        w.spans(&t.path_spans);
        w.bool(t.array);
        w.site(t.site);
        w.len(t.entries.len());
        for e in &t.entries {
            w.strs(&e.key);
            w.spans(&e.key_spans);
            w.site(e.site);
            w.value(&e.value);
        }
    }
    w.0
}

pub fn decode_document(bytes: &[u8]) -> Option<Document> {
    let mut r = R { b: bytes, i: 0 };
    if r.u8()? != VERSION {
        return None;
    }
    let file = FileId(r.u64()?);
    let n = r.len()?;
    let mut tables = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let path = r.strs()?;
        let path_spans = r.spans()?;
        let array = r.bool()?;
        let site = r.site()?;
        let m = r.len()?;
        let mut entries = Vec::with_capacity(m.min(1024));
        for _ in 0..m {
            entries.push(Entry {
                key: r.strs()?,
                key_spans: r.spans()?,
                site: r.site()?,
                value: r.value()?,
            });
        }
        tables.push(Table { path, path_spans, array, site, entries });
    }
    // 余りがある場合は形式が合っていない。読めたところまでを使わない。
    if r.i != r.b.len() {
        return None;
    }
    Some(Document { file, tables })
}

// --- 書き出し ------------------------------------------------------------

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.0.push(v as u8);
    }
    fn len(&mut self, v: usize) {
        self.u32(v as u32);
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }
    fn strs(&mut self, v: &[String]) {
        self.len(v.len());
        for s in v {
            self.str(s);
        }
    }
    fn span(&mut self, s: Span) {
        self.u32(s.start);
        self.u32(s.end);
    }
    fn spans(&mut self, v: &[Span]) {
        self.len(v.len());
        for s in v {
            self.span(*s);
        }
    }
    fn site(&mut self, s: Site) {
        self.u64(s.file.0);
        self.span(s.span);
    }
    fn opt_site(&mut self, s: Option<Site>) {
        match s {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.site(s);
            }
        }
    }

    fn ty(&mut self, t: &Type) {
        match t {
            Type::Str => self.u8(0),
            Type::Int => self.u8(1),
            Type::Bool => self.u8(2),
            Type::Path => self.u8(3),
            Type::DepRef => self.u8(4),
            Type::TargetRef => self.u8(5),
            Type::AbiLabel => self.u8(6),
            Type::Val => self.u8(7),
            Type::Unknown => self.u8(8),
            Type::List(e) => {
                self.u8(9);
                self.ty(e);
            }
            Type::Set(e) => {
                self.u8(10);
                self.ty(e);
            }
            Type::Map(e) => {
                self.u8(11);
                self.ty(e);
            }
            Type::Cfg(e) => {
                self.u8(12);
                self.ty(e);
            }
        }
    }

    fn cfg_key(&mut self, k: &CfgKey) {
        self.u8(match k.ns {
            Ns::Cfg => 0,
            Ns::Host => 1,
            Ns::Feature => 2,
            Ns::Tc => 3,
        });
        self.str(&k.name);
    }

    fn data(&mut self, d: &Data) {
        match d {
            Data::Str(s) => {
                self.u8(0);
                self.str(s);
            }
            Data::Int(v) => {
                self.u8(1);
                self.i64(*v);
            }
            Data::Bool(v) => {
                self.u8(2);
                self.bool(*v);
            }
            Data::Path(p) => {
                self.u8(3);
                self.u8(match p.base {
                    PathBase::Package => 0,
                    PathBase::BuildDir => 1,
                    PathBase::Sysroot => 2,
                });
                self.str(&p.rel);
            }
            Data::Glob(s) => {
                self.u8(4);
                self.str(s);
            }
            Data::Dep(s) => {
                self.u8(5);
                self.str(s);
            }
            Data::Target(s) => {
                self.u8(6);
                self.str(s);
            }
            Data::List(items) => {
                self.u8(7);
                self.len(items.len());
                for v in items {
                    self.value(v);
                }
            }
            Data::Map(m) => {
                self.u8(8);
                self.len(m.len());
                for (k, v) in m {
                    self.str(k);
                    self.value(v);
                }
            }
            Data::Match { scrutinee, arms } => {
                self.u8(9);
                self.cfg_key(scrutinee);
                self.len(arms.len());
                for a in arms {
                    match &a.pattern {
                        Pattern::Wildcard => self.u8(0),
                        Pattern::Value(s) => {
                            self.u8(1);
                            self.str(s);
                        }
                    }
                    self.site(a.site);
                    self.value(&a.value);
                }
            }
            Data::When { pred, inner } => {
                self.u8(10);
                match pred {
                    Pred::Flag(k) => {
                        self.u8(0);
                        self.cfg_key(k);
                    }
                    Pred::Eq(k, v) => {
                        self.u8(1);
                        self.cfg_key(k);
                        self.str(v);
                    }
                }
                self.value(inner);
            }
            Data::Error => self.u8(11),
        }
    }

    fn origin(&mut self, o: &Origin) {
        match o {
            Origin::Literal => self.u8(0),
            Origin::Call(s) => {
                self.u8(1);
                self.str(s);
            }
            Origin::MatchArm(s) => {
                self.u8(2);
                self.str(s);
            }
            Origin::WhenTrue(s) => {
                self.u8(3);
                self.str(s);
            }
            Origin::Propagated { from, prop } => {
                self.u8(4);
                self.str(from);
                self.str(prop);
            }
            Origin::Merged { prop, rule } => {
                self.u8(5);
                self.str(prop);
                self.str(rule);
            }
            Origin::Config => self.u8(6),
            Origin::Default => self.u8(7),
        }
    }

    /// 来歴は自分から根へ向かう順で並べる。復元は逆順に積む。
    fn prov(&mut self, p: &Prov) {
        let chain = p.chain();
        self.len(chain.len());
        for (origin, site) in &chain {
            self.origin(origin);
            self.opt_site(*site);
        }
    }

    fn value(&mut self, v: &Value) {
        self.ty(&v.ty);
        self.data(&v.data);
        self.prov(&v.prov);
    }
}

// --- 読み込み ------------------------------------------------------------

struct R<'a> {
    b: &'a [u8],
    i: usize,
}

impl R<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.i.checked_add(n)?;
        let out = self.b.get(self.i..end)?;
        self.i = end;
        Some(out)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }
    fn len(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        // 長さが残りのバイト数を超える場合、続きは読めない。ここで止めることで、
        // 壊れた入力に対して巨大な確保を試みない。
        if n > self.b.len() - self.i.min(self.b.len()) + self.b.len() {
            return None;
        }
        Some(n)
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        Some(std::str::from_utf8(self.take(n)?).ok()?.to_string())
    }
    fn strs(&mut self) -> Option<Vec<String>> {
        let n = self.len()?;
        let mut out = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            out.push(self.str()?);
        }
        Some(out)
    }
    fn span(&mut self) -> Option<Span> {
        Some(Span::new(self.u32()?, self.u32()?))
    }
    fn spans(&mut self) -> Option<Vec<Span>> {
        let n = self.len()?;
        let mut out = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            out.push(self.span()?);
        }
        Some(out)
    }
    fn site(&mut self) -> Option<Site> {
        Some(Site { file: FileId(self.u64()?), span: self.span()? })
    }
    fn opt_site(&mut self) -> Option<Option<Site>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.site()?)),
            _ => None,
        }
    }

    fn ty(&mut self) -> Option<Type> {
        Some(match self.u8()? {
            0 => Type::Str,
            1 => Type::Int,
            2 => Type::Bool,
            3 => Type::Path,
            4 => Type::DepRef,
            5 => Type::TargetRef,
            6 => Type::AbiLabel,
            7 => Type::Val,
            8 => Type::Unknown,
            9 => Type::List(Box::new(self.ty()?)),
            10 => Type::Set(Box::new(self.ty()?)),
            11 => Type::Map(Box::new(self.ty()?)),
            12 => Type::Cfg(Box::new(self.ty()?)),
            _ => return None,
        })
    }

    fn cfg_key(&mut self) -> Option<CfgKey> {
        let ns = match self.u8()? {
            0 => Ns::Cfg,
            1 => Ns::Host,
            2 => Ns::Feature,
            3 => Ns::Tc,
            _ => return None,
        };
        Some(CfgKey { ns, name: self.str()? })
    }

    fn data(&mut self) -> Option<Data> {
        Some(match self.u8()? {
            0 => Data::Str(self.str()?),
            1 => Data::Int(self.i64()?),
            2 => Data::Bool(self.bool()?),
            3 => {
                let base = match self.u8()? {
                    0 => PathBase::Package,
                    1 => PathBase::BuildDir,
                    2 => PathBase::Sysroot,
                    _ => return None,
                };
                Data::Path(PathValue { base, rel: self.str()? })
            }
            4 => Data::Glob(self.str()?),
            5 => Data::Dep(self.str()?),
            6 => Data::Target(self.str()?),
            7 => {
                let n = self.len()?;
                let mut out = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    out.push(self.value()?);
                }
                Data::List(out)
            }
            8 => {
                let n = self.len()?;
                let mut m = std::collections::BTreeMap::new();
                for _ in 0..n {
                    let k = self.str()?;
                    m.insert(k, self.value()?);
                }
                Data::Map(m)
            }
            9 => {
                let scrutinee = self.cfg_key()?;
                let n = self.len()?;
                let mut arms = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    let pattern = match self.u8()? {
                        0 => Pattern::Wildcard,
                        1 => Pattern::Value(self.str()?),
                        _ => return None,
                    };
                    let site = self.site()?;
                    arms.push(MatchArm { pattern, site, value: self.value()? });
                }
                Data::Match { scrutinee, arms }
            }
            10 => {
                let pred = match self.u8()? {
                    0 => Pred::Flag(self.cfg_key()?),
                    1 => Pred::Eq(self.cfg_key()?, self.str()?),
                    _ => return None,
                };
                Data::When { pred, inner: Box::new(self.value()?) }
            }
            11 => Data::Error,
            _ => return None,
        })
    }

    fn origin(&mut self) -> Option<Origin> {
        Some(match self.u8()? {
            0 => Origin::Literal,
            1 => Origin::Call(self.str()?),
            2 => Origin::MatchArm(self.str()?),
            3 => Origin::WhenTrue(self.str()?),
            4 => Origin::Propagated { from: self.str()?, prop: self.str()? },
            5 => Origin::Merged { prop: self.str()?, rule: merge_rule(&self.str()?) },
            6 => Origin::Config,
            7 => Origin::Default,
            _ => return None,
        })
    }

    fn prov(&mut self) -> Option<Prov> {
        let n = self.len()?;
        let mut chain = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            chain.push((self.origin()?, self.opt_site()?));
        }
        // 鎖は自分から根への順で並んでいる。根から積み直す。
        let mut prov = Prov::none();
        for (origin, site) in chain.into_iter().rev() {
            prov = prov.then(origin, site);
        }
        Some(prov)
    }

    fn value(&mut self) -> Option<Value> {
        Some(Value { ty: self.ty()?, data: self.data()?, prov: self.prov()? })
    }
}

/// 併合規則の名前を `&'static str` に戻す。
///
/// `Origin::Merged` の `rule` が `&'static str` であるため、復元時に
/// 静的な文字列へ写す必要がある。既知の規則名に一致しない場合は
/// `"unknown"` とする。表示にのみ使う値であり、一致しない状況は
/// 形式の版が合っていない場合に限られる。
fn merge_rule(s: &str) -> &'static str {
    for m in [
        crate::schema::Merge::Union,
        crate::schema::Merge::Append,
        crate::schema::Merge::ErrorOnConflict,
        crate::schema::Merge::MustEqual,
        crate::schema::Merge::Replace,
    ] {
        if m.name() == s {
            return m.name();
        }
    }
    "unknown"
}

#[cfg(test)]
mod tests;
