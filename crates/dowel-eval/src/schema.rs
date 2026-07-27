//! スキーマと併合意味論。
//!
//! 「D の実質はここにある」（docs/10-manifest.md 3節）。プロパティごとに
//! 併合規則を**型として**宣言し、プロパティを追加しても検証コードを書き足さなくてよい形にする。

use crate::value::{Data, Origin, Prov, Site, Type, Value};
use dowel_support::{Diagnostic, Label, SourceMap};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Merge {
    /// 和集合。順序は依存グラフのトポロジカル順
    Union,
    /// 連結。順序を保存する
    Append,
    /// 異なる値が到達したら両方の来歴を提示して失敗
    ErrorOnConflict,
    /// 一致しなければ失敗。ABI ラベルの検証はこれで表現される
    MustEqual,
    /// 後勝ち。伝播しないプロパティにのみ用いる
    Replace,
}

impl Merge {
    pub fn name(self) -> &'static str {
        match self {
            Merge::Union => "union",
            Merge::Append => "append",
            Merge::ErrorOnConflict => "error_on_conflict",
            Merge::MustEqual => "must_equal",
            Merge::Replace => "replace",
        }
    }
}

/// ターゲットの種別。閉じた語彙であり、未知の種別は型検査で落ちる。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TableKind {
    Lib,
    Bin,
    Test,
    Bench,
    /// 再利用単位（非再帰）。未実装
    Template,
    /// ツールチェーン記述。未実装
    Toolchain,
    /// 実行ラッパ。未実装
    Runner,
}

impl TableKind {
    pub fn parse(s: &str) -> Option<TableKind> {
        match s {
            "lib" => Some(TableKind::Lib),
            "bin" => Some(TableKind::Bin),
            "test" => Some(TableKind::Test),
            "bench" => Some(TableKind::Bench),
            "template" => Some(TableKind::Template),
            "toolchain" => Some(TableKind::Toolchain),
            "runner" => Some(TableKind::Runner),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TableKind::Lib => "lib",
            TableKind::Bin => "bin",
            TableKind::Test => "test",
            TableKind::Bench => "bench",
            TableKind::Template => "template",
            TableKind::Toolchain => "toolchain",
            TableKind::Runner => "runner",
        }
    }

    /// 成果物を生成する種別か。
    pub fn is_target(self) -> bool {
        matches!(self, TableKind::Lib | TableKind::Bin | TableKind::Test | TableKind::Bench)
    }

    /// 現時点で実装しているか。実装していない種別は診断で明示する。
    pub fn is_implemented(self) -> bool {
        matches!(self, TableKind::Lib | TableKind::Bin | TableKind::Test)
    }

    pub const ALL: &'static [TableKind] = &[
        TableKind::Lib,
        TableKind::Bin,
        TableKind::Test,
        TableKind::Bench,
        TableKind::Template,
        TableKind::Toolchain,
        TableKind::Runner,
    ];
}

/// `public` / `private` の区別。CMake の `INTERFACE` / `PRIVATE` に相当するが、
/// プロパティ名ごとの修飾ではなくブロックで区切る（docs/10-manifest.md 2節）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Block {
    /// ターゲット直下。伝播しない
    Root,
    /// 依存側へ伝播する
    Public,
    /// 自身のコンパイルにのみ効く
    Private,
}

impl Block {
    pub fn parse(s: &str) -> Option<Block> {
        match s {
            "public" => Some(Block::Public),
            "private" => Some(Block::Private),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Block::Root => "(root)",
            Block::Public => "public",
            Block::Private => "private",
        }
    }
}

pub struct PropDef {
    pub name: &'static str,
    pub ty: Type,
    pub merge: Merge,
    pub doc: &'static str,
}

fn list(t: Type) -> Type {
    Type::List(Box::new(t))
}

fn set(t: Type) -> Type {
    Type::Set(Box::new(t))
}

/// ターゲット直下に置けるプロパティ。
pub fn root_props() -> Vec<PropDef> {
    vec![PropDef {
        name: "sources",
        ty: list(Type::Path),
        merge: Merge::Append,
        doc: "コンパイル対象。伝播しない",
    }]
}

/// `public` / `private` ブロックに置けるプロパティ。
///
/// 両ブロックで同じ集合とする。伝播するか否かはブロックが決めるのであって
/// プロパティが決めるのではない。
pub fn block_props() -> Vec<PropDef> {
    vec![
        PropDef {
            name: "includes",
            ty: set(Type::Path),
            merge: Merge::Union,
            doc: "インクルード探索パス。順序は依存グラフのトポロジカル順",
        },
        PropDef {
            name: "defines",
            ty: Type::Map(Box::new(Type::Val)),
            merge: Merge::ErrorOnConflict,
            doc: "プリプロセッサ定義。異なる値が到達したら失敗する",
        },
        PropDef {
            name: "flags",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "コンパイルフラグ。順序を保存する",
        },
        PropDef {
            name: "link_flags",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "リンクフラグ。順序を保存する",
        },
        PropDef {
            name: "deps",
            ty: list(Type::Unknown),
            merge: Merge::Append,
            doc: "依存。dep(...) はパッケージ依存、target(...) は同一パッケージ内",
        },
        PropDef {
            name: "abi",
            ty: Type::AbiLabel,
            merge: Merge::MustEqual,
            doc: "ABI ラベル。一致しなければリンク前に失敗する",
        },
    ]
}

pub fn lookup(block: Block, name: &str) -> Option<PropDef> {
    let props = if block == Block::Root { root_props() } else { block_props() };
    props.into_iter().find(|p| p.name == name)
}

pub fn prop_names(block: Block) -> Vec<&'static str> {
    let props = if block == Block::Root { root_props() } else { block_props() };
    props.into_iter().map(|p| p.name).collect()
}

/// 到達した値を1つに畳む。
///
/// 引数は「到達順」に並んでいること。`Union` と `Append` はこの順序を保存する。
pub fn merge_values(
    def: &PropDef,
    values: &[Value],
    sm: &SourceMap,
    diags: &mut Vec<Diagnostic>,
) -> Value {
    let prov_of = |v: &Value| v.prov.clone();
    let merged_prov = |rule: &'static str, first: Option<&Value>| match first {
        Some(v) => prov_of(v)
            .then(Origin::Merged { prop: def.name.to_string(), rule }, v.prov.nearest_site()),
        None => Prov::new(Origin::Default, None),
    };

    match def.merge {
        Merge::Union => {
            let mut out: Vec<Value> = Vec::new();
            for v in values {
                for item in flatten(v) {
                    // 同値の重複を落とす。来歴は最初に到達したものを残す。
                    if !out.iter().any(|e| e.data == item.data) {
                        out.push(item);
                    }
                }
            }
            Value {
                ty: def.ty.clone(),
                data: Data::List(out),
                prov: merged_prov("union", values.first()),
            }
        }
        Merge::Append => {
            let mut out: Vec<Value> = Vec::new();
            for v in values {
                out.extend(flatten(v));
            }
            Value {
                ty: def.ty.clone(),
                data: Data::List(out),
                prov: merged_prov("append", values.first()),
            }
        }
        Merge::ErrorOnConflict => {
            let mut out: BTreeMap<String, Value> = BTreeMap::new();
            for v in values {
                let Data::Map(m) = &v.data else { continue };
                for (k, item) in m {
                    if let Some(prev) = out.get(k) {
                        if prev.data != item.data {
                            diags.push(conflict_diagnostic(def.name, k, prev, item, sm));
                            continue;
                        }
                    }
                    out.insert(k.clone(), item.clone());
                }
            }
            Value {
                ty: def.ty.clone(),
                data: Data::Map(out),
                prov: merged_prov("error_on_conflict", values.first()),
            }
        }
        Merge::MustEqual => {
            let mut iter = values.iter().filter(|v| !v.is_error());
            let Some(first) = iter.next() else {
                return Value { ty: def.ty.clone(), data: Data::Error, prov: Prov::none() };
            };
            for v in iter {
                if v.data != first.data {
                    diags.push(must_equal_diagnostic(def.name, first, v, sm));
                }
            }
            first.clone()
        }
        Merge::Replace => values.last().cloned().unwrap_or_else(|| Value {
            ty: def.ty.clone(),
            data: Data::Error,
            prov: Prov::none(),
        }),
    }
}

/// 列の値なら要素を、そうでなければ自身を1要素として返す。
fn flatten(v: &Value) -> Vec<Value> {
    match &v.data {
        Data::List(items) => items.clone(),
        Data::Error => Vec::new(),
        _ => vec![v.clone()],
    }
}

fn site_label(_sm: &SourceMap, site: Option<Site>, msg: &str) -> Option<Label> {
    site.map(|s| Label::secondary(s.file, s.span, msg.to_string()))
}

fn conflict_diagnostic(
    prop: &str,
    key: &str,
    prev: &Value,
    cur: &Value,
    sm: &SourceMap,
) -> Diagnostic {
    let mut d =
        Diagnostic::error("merge-conflict", format!("`{prop}` の `{key}` に異なる値が到達した"))
            .note(format!("`{prop}` の併合規則は error_on_conflict である"));
    if let Some(s) = cur.prov.nearest_site() {
        d = d.at(s.file, s.span, format!("こちらは {}", cur.display()));
    }
    if let Some(l) =
        site_label(sm, prev.prov.nearest_site(), &format!("先に到達した値は {}", prev.display()))
    {
        d = d.with_label(l);
    }
    for line in provenance_notes(prev).into_iter().chain(provenance_notes(cur)) {
        d = d.note(line);
    }
    d
}

fn must_equal_diagnostic(prop: &str, first: &Value, other: &Value, sm: &SourceMap) -> Diagnostic {
    let mut d = Diagnostic::error(
        "abi-mismatch",
        format!("`{prop}` が一致しない: {} と {}", first.display(), other.display()),
    )
    .note(format!("`{prop}` の併合規則は must_equal である。不整合は伝播させず失敗させる"));
    if let Some(s) = other.prov.nearest_site() {
        d = d.at(s.file, s.span, format!("こちらは {}", other.display()));
    }
    if let Some(l) =
        site_label(sm, first.prov.nearest_site(), &format!("先に到達した値は {}", first.display()))
    {
        d = d.with_label(l);
    }
    d
}

/// 来歴の鎖を注記の行へ変換する。診断だけで伝播経路が読めるようにする。
fn provenance_notes(v: &Value) -> Vec<String> {
    v.prov.chain().iter().skip(1).map(|(origin, _)| format!("  ← {}", origin.display())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dowel_support::{FileId, Span};

    fn site(start: u32) -> Site {
        Site::new(FileId(0), Span::new(start, start + 3))
    }

    fn path(rel: &str, at: u32) -> Value {
        Value {
            ty: Type::Path,
            data: Data::Path(crate::value::PathValue {
                base: crate::value::PathBase::Package,
                rel: rel.into(),
            }),
            prov: Prov::at(Origin::Call("dir".into()), site(at)),
        }
    }

    fn map_of(pairs: &[(&str, i64)], at: u32) -> Value {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(
                k.to_string(),
                Value {
                    ty: Type::Int,
                    data: Data::Int(*v),
                    prov: Prov::at(Origin::Literal, site(at)),
                },
            );
        }
        Value { ty: Type::Map(Box::new(Type::Val)), data: Data::Map(m), prov: Prov::none() }
    }

    #[test]
    fn union_は重複を落とし到達順を保つ() {
        let def = lookup(Block::Public, "includes").unwrap();
        let a = Value::list(Type::Path, vec![path("include", 0), path("src", 10)], Prov::none());
        let b = Value::list(Type::Path, vec![path("src", 20), path("gen", 30)], Prov::none());
        let mut diags = Vec::new();
        let merged = merge_values(&def, &[a, b], &SourceMap::new(), &mut diags);
        assert!(diags.is_empty());
        let items = merged.as_list().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].display(), "include");
        assert_eq!(items[1].display(), "src");
        assert_eq!(items[2].display(), "gen");
    }

    #[test]
    fn append_は重複も順序も保つ() {
        let def = lookup(Block::Private, "flags").unwrap();
        let f = |s: &str, at: u32| Value::str(s, Prov::at(Origin::Literal, site(at)));
        let a = Value::list(Type::Str, vec![f("-O2", 0), f("-g", 5)], Prov::none());
        let b = Value::list(Type::Str, vec![f("-O2", 10)], Prov::none());
        let mut diags = Vec::new();
        let merged = merge_values(&def, &[a, b], &SourceMap::new(), &mut diags);
        assert_eq!(merged.as_list().unwrap().len(), 3);
    }

    #[test]
    fn error_on_conflict_は異なる値で失敗する() {
        let def = lookup(Block::Private, "defines").unwrap();
        let mut diags = Vec::new();
        let sm = SourceMap::new();
        merge_values(&def, &[map_of(&[("A", 1)], 0), map_of(&[("A", 1)], 9)], &sm, &mut diags);
        assert!(diags.is_empty(), "同じ値なら衝突しない");
        merge_values(&def, &[map_of(&[("A", 1)], 0), map_of(&[("A", 2)], 9)], &sm, &mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "merge-conflict");
    }

    #[test]
    fn must_equal_は不一致で失敗する() {
        let def = lookup(Block::Public, "abi").unwrap();
        let label = |s: &str, at: u32| Value {
            ty: Type::AbiLabel,
            data: Data::Str(s.into()),
            prov: Prov::at(Origin::Literal, site(at)),
        };
        let mut diags = Vec::new();
        let sm = SourceMap::new();
        merge_values(&def, &[label("gnu11", 0), label("gnu11", 9)], &sm, &mut diags);
        assert!(diags.is_empty());
        merge_values(&def, &[label("gnu11", 0), label("cxx11abi0", 9)], &sm, &mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "abi-mismatch");
    }

    #[test]
    fn 既知のプロパティ名を列挙できる() {
        assert!(prop_names(Block::Public).contains(&"includes"));
        assert!(prop_names(Block::Root).contains(&"sources"));
        assert!(!prop_names(Block::Root).contains(&"includes"));
    }
}
