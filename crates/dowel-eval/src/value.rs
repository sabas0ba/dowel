//! 型つき値と来歴。
//!
//! `Value = { type, data, provenance }`（docs/20-architecture.md 4節）。
//! 来歴を副次情報ではなく値の構成要素として持つ。`dowel why` はこの鎖を辿るだけであり、
//! 専用のデータ構造を持たない。

use dowel_support::{FileId, Span};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 値の型。
///
/// `Path` を `Str` と別型にするのは、文字列連結によるパス構築を
/// 言語として提供しないためである（docs/10-manifest.md 3節）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Type {
    Str,
    Int,
    Bool,
    /// 基準点を型に含むパス。
    Path,
    /// `dep("bar")` — パッケージ依存への参照
    DepRef,
    /// `target("foo")` — 同一パッケージ内のターゲットへの参照
    TargetRef,
    /// ABI ラベル。`must_equal` で検証される。
    /// 現時点では文字列で書く。算出は Phase 6（docs/90-roadmap.md）
    AbiLabel,
    /// スカラ値（`Str` / `Int` / `Bool` のいずれか）。
    /// `defines` のように「値の種類を問わない」プロパティのための型
    Val,
    /// コマンドラインの1語。`Str` そのもの、または `Path`（絶対パスへ
    /// 展開される）。道を要するフラグを、文字列連結なしに書くための型
    /// （issue #70）
    Word,
    List(Box<Type>),
    Set(Box<Type>),
    Map(Box<Type>),
    /// 構成でパラメタライズされた型。`match` の結果が持つ。
    /// 具体化は plan 時であり、`--release` の切り替えでマニフェスト評価を
    /// やり直さないための型である（docs/10-manifest.md 3節）。
    Cfg(Box<Type>),
    /// 型検査を通せなかった値。診断は既に出ており、下流は伝播させるだけ。
    Unknown,
}

impl Type {
    pub fn display(&self) -> String {
        match self {
            Type::Str => "Str".into(),
            Type::Int => "Int".into(),
            Type::Bool => "Bool".into(),
            Type::Path => "Path".into(),
            Type::DepRef => "DepRef".into(),
            Type::TargetRef => "TargetRef".into(),
            Type::AbiLabel => "AbiLabel".into(),
            Type::Val => "Val".into(),
            Type::Word => "Str | Path".into(),
            Type::List(t) => format!("List<{}>", t.display()),
            Type::Set(t) => format!("Set<{}>", t.display()),
            Type::Map(t) => format!("Map<Ident, {}>", t.display()),
            Type::Cfg(t) => format!("Cfg<{}>", t.display()),
            Type::Unknown => "?".into(),
        }
    }

    /// 要素型。列でなければ `None`。
    pub fn elem(&self) -> Option<&Type> {
        match self {
            Type::List(t) | Type::Set(t) | Type::Map(t) => Some(t),
            _ => None,
        }
    }

    /// `Cfg<T>` の皮を剥いだ型。
    pub fn concrete(&self) -> &Type {
        match self {
            Type::Cfg(t) => t.concrete(),
            t => t,
        }
    }

    /// 代入互換か。`Unknown` は誤りの伝播を止めるため常に互換とする。
    pub fn accepts(&self, other: &Type) -> bool {
        if matches!(self, Type::Unknown) || matches!(other, Type::Unknown) {
            return true;
        }
        match (self, other) {
            (Type::Cfg(a), b) => a.accepts(b),
            // ABI ラベルは現状スカラ文字列として書く。
            (Type::AbiLabel, Type::Str) => true,
            // Val はスカラを受ける。
            (Type::Val, Type::Str | Type::Int | Type::Bool | Type::Val) => true,
            // 語は文字列と道の双方を受ける。道は絶対パスへ展開される。
            (Type::Word, Type::Str | Type::Path | Type::Word) => true,
            (a, Type::Cfg(b)) => a.accepts(b),
            (Type::List(a), Type::List(b)) | (Type::Set(a), Type::Set(b)) => a.accepts(b),
            (Type::Set(a), Type::List(b)) | (Type::List(a), Type::Set(b)) => a.accepts(b),
            (Type::Map(a), Type::Map(b)) => a.accepts(b),
            // パスは文字列から作らない。glob / dir / file を経由させる。
            (a, b) => a == b,
        }
    }
}

/// パスの基準点。文字列としてのパスを持たないことの実体。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathBase {
    /// このマニフェストが属するパッケージのルート
    Package,
    /// 構成ごとのビルドディレクトリ
    BuildDir,
    /// ツールチェーンの sysroot
    Sysroot,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PathValue {
    pub base: PathBase,
    /// 基準点からの相対パス。区切りは `/` に正規化する。
    pub rel: String,
}

impl PathValue {
    pub fn display(&self) -> String {
        match self.base {
            PathBase::Package => self.rel.clone(),
            PathBase::BuildDir => format!("<build>/{}", self.rel),
            PathBase::Sysroot => format!("<sysroot>/{}", self.rel),
        }
    }
}

/// 構成の名前空間。Q1 が未決のため閉じた語彙として実装で仮置きしている
/// （docs/99-open-questions.md Q1）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ns {
    Cfg,
    Host,
    Feature,
    Tc,
    /// パッケージの定数（[ADR-0020](../../../docs/adr/0020-package-constants.md)）。
    /// 構成の軸ではない——`match` の被検査対象にも `when` の述語にもならず、
    /// 代わりに値の位置に書ける
    Pkg,
}

impl Ns {
    pub fn parse(s: &str) -> Option<Ns> {
        match s {
            "cfg" => Some(Ns::Cfg),
            "host" => Some(Ns::Host),
            "feature" => Some(Ns::Feature),
            "tc" => Some(Ns::Tc),
            "pkg" => Some(Ns::Pkg),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Ns::Cfg => "cfg",
            Ns::Host => "host",
            Ns::Feature => "feature",
            Ns::Tc => "tc",
            Ns::Pkg => "pkg",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CfgKey {
    pub ns: Ns,
    pub name: String,
}

impl CfgKey {
    pub fn display(&self) -> String {
        format!("{}.{}", self.ns.name(), self.name)
    }
}

/// `when` の述語。合成は暗黙の AND のみ（Q1 の暫定）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Pred {
    /// `when feature.zlib` — 真偽として読む
    Flag(CfgKey),
    /// `when cfg.opt == "release"`
    Eq(CfgKey, String),
}

impl Pred {
    pub fn display(&self) -> String {
        match self {
            Pred::Flag(k) => k.display(),
            Pred::Eq(k, v) => format!("{} == {:?}", k.display(), v),
        }
    }

    pub fn key(&self) -> &CfgKey {
        match self {
            Pred::Flag(k) | Pred::Eq(k, _) => k,
        }
    }
}

/// `match` のアーム左辺。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Pattern {
    Value(String),
    Wildcard,
}

impl Pattern {
    pub fn display(&self) -> String {
        match self {
            Pattern::Value(v) => v.clone(),
            Pattern::Wildcard => "_".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub value: Value,
    pub site: Site,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Data {
    Str(String),
    Int(i64),
    Bool(bool),
    Path(PathValue),
    /// `glob("src/**.c")`。ファイル走査は plan 時に行う。
    /// 評価時に走査すると、記録されない入力（その時点のファイルシステム）が
    /// 評価結果に混ざる。
    Glob(String),
    Dep(String),
    Target(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
    /// 構成で分岐する値。具体化まで保持する。
    Match {
        scrutinee: CfgKey,
        arms: Vec<MatchArm>,
    },
    /// 述語つきの値。具体化で残るか消える。
    When {
        pred: Pred,
        inner: Box<Value>,
    },
    /// パッケージの定数への参照（ADR-0020）。具体化まで保持する。
    ///
    /// 評価時に埋めない。評価の結果はファイルの内容で鍵付けして保存されるが、
    /// `dowel.toml` の版が動いても `dowel.build` の内容は変わらない。
    /// 評価時に埋めると、古い版が保存され、issue #80 が裏側から戻ってくる
    PkgRef(String),
    /// 誤りの位置。診断は既に出ている。
    Error,
}

/// ソース上の位置。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Site {
    pub file: FileId,
    pub span: Span,
}

impl Site {
    pub fn new(file: FileId, span: Span) -> Site {
        Site { file, span }
    }
}

/// 値がその形になった理由。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Origin {
    /// ソースに直接書かれている
    Literal,
    /// `glob(...)` などの呼び出しの結果
    Call(String),
    /// `match` の選ばれたアーム
    MatchArm(String),
    /// `when` の述語が成立した
    WhenTrue(String),
    /// 依存の公開プロパティから伝播した
    Propagated { from: String, prop: String },
    /// 複数の到達値を併合した
    Merged { prop: String, rule: &'static str },
    /// 構成から与えられた
    Config,
    /// 実装が与えた既定値
    Default,
}

impl Origin {
    pub fn display(&self) -> String {
        match self {
            Origin::Literal => "literal".into(),
            Origin::Call(f) => format!("{f}(...)"),
            Origin::MatchArm(p) => format!("match arm `{p}`"),
            Origin::WhenTrue(p) => format!("when {p}"),
            Origin::Propagated { from, prop } => format!("{prop} of {from}"),
            Origin::Merged { prop, rule } => format!("merge of {prop} ({rule})"),
            Origin::Config => "configuration".into(),
            Origin::Default => "default".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct ProvNode {
    origin: Origin,
    site: Option<Site>,
    parent: Prov,
}

/// 来歴の鎖。値のコピーが多いため `Arc` で共有する。
///
/// クエリグラフの部分木の射影であり、増分評価エンジンを実装していれば
/// これ自体は追加のデータ構造を要さない（docs/10-manifest.md 5節）。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Prov(Option<Arc<ProvNode>>);

impl Prov {
    pub fn none() -> Prov {
        Prov(None)
    }

    pub fn new(origin: Origin, site: Option<Site>) -> Prov {
        Prov(Some(Arc::new(ProvNode { origin, site, parent: Prov(None) })))
    }

    pub fn at(origin: Origin, site: Site) -> Prov {
        Prov::new(origin, Some(site))
    }

    /// この来歴を親として、新しい段を積む。
    pub fn then(&self, origin: Origin, site: Option<Site>) -> Prov {
        Prov(Some(Arc::new(ProvNode { origin, site, parent: self.clone() })))
    }

    pub fn origin(&self) -> Option<&Origin> {
        self.0.as_ref().map(|n| &n.origin)
    }

    pub fn site(&self) -> Option<Site> {
        self.0.as_ref().and_then(|n| n.site)
    }

    /// 最も近い位置情報。自分が持たなければ親を辿る。
    pub fn nearest_site(&self) -> Option<Site> {
        let mut cur = self;
        loop {
            let node = cur.0.as_ref()?;
            if let Some(s) = node.site {
                return Some(s);
            }
            cur = &node.parent;
        }
    }

    /// 根に向かう鎖。`dowel why` の出力そのもの。
    pub fn chain(&self) -> Vec<(Origin, Option<Site>)> {
        let mut out = Vec::new();
        let mut cur = self.clone();
        while let Some(node) = cur.0.clone() {
            out.push((node.origin.clone(), node.site));
            cur = node.parent.clone();
        }
        out
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Value {
    pub ty: Type,
    pub data: Data,
    pub prov: Prov,
}

impl Value {
    pub fn new(ty: Type, data: Data, prov: Prov) -> Value {
        Value { ty, data, prov }
    }

    pub fn error(prov: Prov) -> Value {
        Value { ty: Type::Unknown, data: Data::Error, prov }
    }

    pub fn str(s: impl Into<String>, prov: Prov) -> Value {
        Value { ty: Type::Str, data: Data::Str(s.into()), prov }
    }

    pub fn list(ty: Type, items: Vec<Value>, prov: Prov) -> Value {
        Value { ty: Type::List(Box::new(ty)), data: Data::List(items), prov }
    }

    pub fn is_error(&self) -> bool {
        matches!(self.data, Data::Error)
    }

    /// 構成に依存するか。`Cfg<T>` の判定であり、
    /// 具体化前に確定値を要求する箇所の検査に使う。
    pub fn is_conditional(&self) -> bool {
        match &self.data {
            Data::Match { .. } | Data::When { .. } => true,
            Data::List(items) => items.iter().any(|v| v.is_conditional()),
            Data::Map(m) => m.values().any(|v| v.is_conditional()),
            _ => false,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.data {
            Data::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match &self.data {
            Data::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match &self.data {
            Data::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match &self.data {
            Data::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&BTreeMap<String, Value>> {
        match &self.data {
            Data::Map(m) => Some(m),
            _ => None,
        }
    }

    /// 表示用。診断とログに出す。
    pub fn display(&self) -> String {
        match &self.data {
            Data::Str(s) => format!("{s:?}"),
            Data::Int(i) => i.to_string(),
            Data::Bool(b) => b.to_string(),
            Data::Path(p) => p.display(),
            Data::Glob(g) => format!("glob({g:?})"),
            Data::Dep(d) => format!("dep({d:?})"),
            Data::Target(t) => format!("target({t:?})"),
            Data::List(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.display()).collect();
                format!("[{}]", inner.join(", "))
            }
            Data::Map(m) => {
                let inner: Vec<String> =
                    m.iter().map(|(k, v)| format!("{k} = {}", v.display())).collect();
                format!("{{ {} }}", inner.join(", "))
            }
            Data::Match { scrutinee, arms } => {
                let inner: Vec<String> = arms
                    .iter()
                    .map(|a| format!("{} => {}", a.pattern.display(), a.value.display()))
                    .collect();
                format!("match {} {{ {} }}", scrutinee.display(), inner.join(", "))
            }
            Data::When { pred, inner } => format!("{} when {}", inner.display(), pred.display()),
            Data::PkgRef(name) => format!("pkg.{name}"),
            Data::Error => "<error>".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_chain_walks_towards_the_root() {
        let site = Site::new(FileId(0), Span::new(10, 20));
        let base = Prov::at(Origin::Literal, site);
        let p = base
            .then(Origin::Call("glob".into()), None)
            .then(Origin::Propagated { from: "target:foo".into(), prop: "includes".into() }, None);
        let chain = p.chain();
        assert_eq!(chain.len(), 3);
        assert!(matches!(chain[0].0, Origin::Propagated { .. }));
        assert!(matches!(chain[2].0, Origin::Literal));
        // 位置を持たない段でも、辿れば最寄りの位置が得られる。
        assert_eq!(p.nearest_site(), Some(site));
    }

    #[test]
    fn assignment_compatibility() {
        assert!(Type::List(Box::new(Type::Path)).accepts(&Type::List(Box::new(Type::Path))));
        assert!(Type::Set(Box::new(Type::Path)).accepts(&Type::List(Box::new(Type::Path))));
        assert!(!Type::Path.accepts(&Type::Str), "a Path is never built from a Str");
        assert!(Type::Map(Box::new(Type::Val)).accepts(&Type::Map(Box::new(Type::Int))));
        assert!(Type::AbiLabel.accepts(&Type::Str));
        assert!(Type::Path.accepts(&Type::Unknown), "errors must not propagate further");
        assert!(Type::List(Box::new(Type::Str))
            .accepts(&Type::Cfg(Box::new(Type::List(Box::new(Type::Str))))));
    }

    #[test]
    fn detects_conditional_values() {
        let prov = Prov::none();
        let plain = Value::str("a", prov.clone());
        assert!(!plain.is_conditional());
        let cond = Value {
            ty: Type::Str,
            data: Data::When {
                pred: Pred::Flag(CfgKey { ns: Ns::Feature, name: "zlib".into() }),
                inner: Box::new(plain.clone()),
            },
            prov: prov.clone(),
        };
        assert!(cond.is_conditional());
        assert!(Value::list(Type::Str, vec![plain, cond], prov).is_conditional());
    }
}
