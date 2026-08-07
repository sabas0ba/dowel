//! スキーマと併合意味論。
//!
//! 「D の実質はここにある」（docs/10-manifest.md 3節）。プロパティごとに
//! 併合規則を型として宣言し、プロパティを追加しても検証コードを書き足さなくてよい形にする。

use crate::value::{Data, Origin, Prov, Site, Type, Value};
use dowel_support::{Diagnostic, Label};
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
    /// 語彙の順序で最も高いものを採る。順序のある閉じた語彙にのみ用いる。
    ///
    /// 言語標準がこれである。C++17 を要求するライブラリを C++20 の
    /// 実行ファイルから使うのは正しい——閉包の中で最も高い標準で組めば、
    /// どのターゲットの要求も満たされる。`must_equal` にすると、
    /// 依存とこちらで標準が違うだけでビルドが落ちる
    Max,
}

impl Merge {
    pub fn name(self) -> &'static str {
        match self {
            Merge::Union => "union",
            Merge::Append => "append",
            Merge::ErrorOnConflict => "error_on_conflict",
            Merge::MustEqual => "must_equal",
            Merge::Replace => "replace",
            Merge::Max => "max",
        }
    }
}

/// 言語ではなく境界を指す ABI 札（[ADR-0019](../../../docs/adr/0019-c-abi-label.md)）。
///
/// `extern "C"` の面しか持たない公開面はこれを名乗る。C の関数には多重定義も
/// テンプレートもインライン関数の実体化も無く、名前の飾りも付かない。この一線を
/// 跨ぐ呼び出しでは、両側の言語が違っても ODR 違反は起こらない。
///
/// 配る側が利用者の言語を知らないまま札を書けるのは、この形だけである。
/// 言語の札を1つ選ぶと、それを全ての利用者に強制することになる（issue #78）。
pub const C_ABI: &str = "c";

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
    /// 実行ラッパ（docs/30-devexp.md 1節）
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
        matches!(self, TableKind::Lib | TableKind::Bin | TableKind::Test | TableKind::Runner)
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
    /// 閉じた語彙。`Some` なら、この一覧に無い値は診断で落ちる。
    ///
    /// 並びは意味のある順序であり、`Merge::Max` はこの添字を比べる。
    pub domain: Option<&'static [&'static str]>,
}

/// C の言語標準。低い順に並べる（`Merge::Max` がこの添字を比べる）。
///
/// GNU 拡張の方言（`gnu11` 等）は入れない。方言は標準の版とは別の軸であり、
/// 一列に並べられない。必要なら `c_flags = ["-std=gnu11"]` と書く——
/// 後に置かれるため、こちらが勝つ。
pub const C_STANDARDS: &[&str] = &["c89", "c99", "c11", "c17", "c23"];

/// C++ の言語標準。低い順。
pub const CXX_STANDARDS: &[&str] =
    &["c++98", "c++03", "c++11", "c++14", "c++17", "c++20", "c++23", "c++26"];

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
        doc: "sources to compile. does not propagate",
        domain: None,
    }]
}

/// `[runner.<triple>]` に置けるプロパティ（docs/30-devexp.md 1節）。
///
/// ターゲットのプロパティとは別の集合とする。ランナーは成果物を生成せず、
/// 伝播もしない。同一の名前空間に置いた場合、`sources` を持つランナーのような
/// 意味を持たない記述が型検査を通過する。
pub fn runner_props() -> Vec<PropDef> {
    vec![
        PropDef {
            name: "command",
            ty: Type::Str,
            merge: Merge::Replace,
            doc: "the program that wraps the artifact, such as `qemu-riscv64`",
            domain: None,
        },
        PropDef {
            name: "args",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "arguments placed before the artifact path",
            domain: None,
        },
        PropDef {
            name: "transfer",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "command that copies the artifact. the source and destination are appended",
            domain: None,
        },
        PropDef {
            name: "remote_dir",
            ty: Type::Str,
            merge: Merge::Replace,
            doc: "directory on the target machine that receives the artifact",
            domain: None,
        },
        PropDef {
            name: "host",
            ty: Type::Str,
            merge: Merge::Replace,
            doc: "host part of the transfer destination, written as `<host>:<path>`",
            domain: None,
        },
    ]
}

/// `[<kind>.<name>.artifacts]` の1項目に置けるプロパティ（issue #60）。
///
/// 項目そのものはインラインテーブルであり、鍵が出力の拡張子になる。
///
/// ```toml
/// [bin.firmware.artifacts]
/// bin = { tool = "objcopy", args = ["-O", "binary"] }
/// ```
///
/// 入力（元の成果物）と出力は書かせない。書式文字列も置かない。
/// 位置で渡す（[ADR-0008]）——実行される列は
/// `<tool> <args...> <入力> <出力>` である。
///
/// [ADR-0008]: ../../../docs/adr/0008-runner-transfer.md
pub fn artifact_props() -> Vec<PropDef> {
    vec![
        PropDef {
            name: "tool",
            ty: Type::Str,
            merge: Merge::Replace,
            doc: "the toolchain tool that performs the transform, such as `objcopy`",
            domain: None,
        },
        PropDef {
            name: "args",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "arguments placed before the input and output paths",
            domain: None,
        },
    ]
}

/// `[<kind>.<name>.inspect]` の1項目に置けるプロパティ（issue #60）。
///
/// 検査は成果物を作らない。作らないため、増分の対象にも `dowel build` の
/// 既定にもならない——最新かどうかを判定する出力が無い。走らせるのは
/// `dowel inspect` である。
///
/// ```toml
/// [bin.firmware.inspect]
/// size = { tool = "size", args = ["-A"] }
/// ```
///
/// 実行される列は `<tool> <args...> <成果物>` である。成果物の位置は
/// 書かせない（[ADR-0008]）。
///
/// [ADR-0008]: ../../../docs/adr/0008-runner-transfer.md
pub fn inspection_props() -> Vec<PropDef> {
    vec![
        PropDef {
            name: "tool",
            ty: Type::Str,
            merge: Merge::Replace,
            doc: "the toolchain tool that reports, such as `size`",
            domain: None,
        },
        PropDef {
            name: "args",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "arguments placed before the artifact path",
            domain: None,
        },
    ]
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
            doc: "include search paths. ordered along the dependency graph",
            domain: None,
        },
        PropDef {
            name: "defines",
            ty: Type::Map(Box::new(Type::Val)),
            merge: Merge::ErrorOnConflict,
            doc: "preprocessor definitions. fails when conflicting values arrive",
            domain: None,
        },
        PropDef {
            name: "flags",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "compile flags for every language. order preserving",
            domain: None,
        },
        PropDef {
            name: "c_flags",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "compile flags for C sources only, after `flags`. order preserving",
            domain: None,
        },
        PropDef {
            name: "cxx_flags",
            ty: list(Type::Str),
            merge: Merge::Append,
            doc: "compile flags for C++ sources only, after `flags`. order preserving",
            domain: None,
        },
        PropDef {
            name: "link_flags",
            ty: list(Type::Word),
            merge: Merge::Append,
            doc: "link flags, order preserving. a `Path` element expands to its absolute path",
            domain: None,
        },
        PropDef {
            name: "c_std",
            ty: Type::Str,
            merge: Merge::Max,
            doc: "C language standard, such as `c17`. becomes `-std=` for C sources",
            domain: Some(C_STANDARDS),
        },
        PropDef {
            name: "cxx_std",
            ty: Type::Str,
            merge: Merge::Max,
            doc: "C++ language standard, such as `c++20`. becomes `-std=` for C++ sources",
            domain: Some(CXX_STANDARDS),
        },
        PropDef {
            name: "deps",
            ty: list(Type::Unknown),
            merge: Merge::Append,
            doc: "dependencies. dep(...) is a package dependency, target(...) is same-package",
            domain: None,
        },
        PropDef {
            name: "abi",
            ty: Type::AbiLabel,
            merge: Merge::MustEqual,
            doc: "ABI label. mismatches fail before linking; `c` names the C ABI boundary and matches any label",
            domain: None,
        },
    ]
}

/// 組み込み関数の署名と説明。
///
/// `dowel schema dump` と言語サーバのホバーが同じ表を読む。二重に持つと、
/// 片方だけを直したときに黙って食い違う。
pub const FUNCTIONS: &[(&str, &str, &str)] = &[
    ("glob", "(Str) -> List<Path>", "files matching the pattern; expanded at plan time"),
    ("dir", "(Str) -> Path", "a directory relative to the package root"),
    ("file", "(Str) -> Path", "a file relative to the package root"),
    ("dep", "(Str) -> DepRef", "a reference to a dependency declared in dowel.toml"),
    ("target", "(Str) -> TargetRef", "a reference to a target in the same package"),
];

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
pub fn merge_values(def: &PropDef, values: &[Value], diags: &mut Vec<Diagnostic>) -> Value {
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
                    if !out.iter().any(|e| same_item(e, &item)) {
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
                            diags.push(conflict_diagnostic(def.name, k, prev, item));
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
            // 境界を指す札は、どの言語の札とも突き合わせない（ADR-0019）。
            // 除外は ABI 札の語彙が持つ性質であって、`must_equal` の性質では
            // ない——他のプロパティでの `must_equal` は依然「一致」である。
            let exempt = |v: &Value| def.ty == Type::AbiLabel && v.as_str() == Some(C_ABI);
            let mut iter = values.iter().filter(|v| !v.is_error() && !exempt(v));
            let Some(first) = iter.next() else {
                // 全てが `c`、あるいは値が無い。`c` は制約を足さないだけで
                // 消しはしないので、残っているものをそのまま採る。
                return values.iter().find(|v| !v.is_error()).cloned().unwrap_or_else(|| Value {
                    ty: def.ty.clone(),
                    data: Data::Error,
                    prov: Prov::none(),
                });
            };
            for v in iter {
                if v.data != first.data {
                    diags.push(must_equal_diagnostic(def.name, first, v));
                }
            }
            first.clone()
        }
        Merge::Replace => values.last().cloned().unwrap_or_else(|| Value {
            ty: def.ty.clone(),
            data: Data::Error,
            prov: Prov::none(),
        }),
        // 語彙の順で最も高いものを採る。語彙の外は既に診断済みで、ここでは
        // 順序を決められないため最も低いものとして扱う（採られない）。
        Merge::Max => {
            let rank = |v: &Value| {
                v.as_str()
                    .and_then(|s| def.domain.and_then(|d| d.iter().position(|c| *c == s)))
                    .map(|i| i as i64)
                    .unwrap_or(-1)
            };
            values.iter().filter(|v| !v.is_error()).max_by_key(|v| rank(v)).cloned().unwrap_or_else(
                || Value { ty: def.ty.clone(), data: Data::Error, prov: Prov::none() },
            )
        }
    }
}

/// 列の値なら要素を、そうでなければ自身を1要素として返す。
/// 併合での同値判定。
///
/// パスは「パッケージルートからの相対」で表され、基点は値ではなく宣言位置が持つ
/// （docs/10-manifest.md 3節）。したがって同じ `dir("include")` でも、
/// 宣言したファイルが違えば指す先は別のディレクトリである。
///
/// データだけで比べると、依存が2段を超えた途端に別パッケージの
/// インクルードディレクトリが「重複」と見なされて消える。慣習として
/// どのパッケージも公開ヘッダを `include/` に置くため、これは例外ではなく既定の形になる。
fn same_item(a: &Value, b: &Value) -> bool {
    if a.data != b.data {
        return false;
    }
    // 基点を持つ値だけ、宣言位置まで含めて比べる。
    if matches!(a.data, Data::Path(_) | Data::Glob(_)) {
        let site = |v: &Value| v.prov.nearest_site().map(|s| s.file);
        return site(a) == site(b);
    }
    true
}

/// 列の値なら要素へ、そうでなければ自身を1要素として返す。
///
/// 入れ子は最後まで解く。列の要素に `match` を書くと、具体化した結果は
/// 列の中の列になる。1段しか解かないと、その値は併合の結果に残ったまま
/// 下流で読み飛ばされ、`check` も `why` も通るのにコンパイル引数にだけ
/// 現れないという状態になる。
///
/// 要素型はいずれもスカラであり（`List<Str>` / `List<Path>` / `List<DepRef>`）、
/// 入れ子そのものに意味は無い。評価は全域で再帰を持たないため
/// （[ADR-0004](../../../docs/adr/0004-syntax.md)）、値は有限の木である。
fn flatten(v: &Value) -> Vec<Value> {
    match &v.data {
        Data::List(items) => items.iter().flat_map(flatten).collect(),
        Data::Error => Vec::new(),
        _ => vec![v.clone()],
    }
}

fn site_label(site: Option<Site>, msg: &str) -> Option<Label> {
    site.map(|s| Label::secondary(s.file, s.span, msg.to_string()))
}

fn conflict_diagnostic(prop: &str, key: &str, prev: &Value, cur: &Value) -> Diagnostic {
    let mut d = Diagnostic::error(
        "merge-conflict",
        format!("conflicting values reached `{key}` of `{prop}`"),
    )
    .note(format!("the merge rule of `{prop}` is error_on_conflict"));
    if let Some(s) = cur.prov.nearest_site() {
        d = d.at(s.file, s.span, format!("this one is {}", cur.display()));
    }
    if let Some(l) = site_label(
        prev.prov.nearest_site(),
        &format!("the value that arrived first is {}", prev.display()),
    ) {
        d = d.with_label(l);
    }
    for line in provenance_notes(prev).into_iter().chain(provenance_notes(cur)) {
        d = d.note(line);
    }
    d
}

fn must_equal_diagnostic(prop: &str, first: &Value, other: &Value) -> Diagnostic {
    let mut d = Diagnostic::error(
        "abi-mismatch",
        format!("`{prop}` does not match: {} vs {}", first.display(), other.display()),
    )
    .note(format!(
        "the merge rule of `{prop}` is must_equal. a mismatch fails instead of propagating"
    ));
    if let Some(s) = other.prov.nearest_site() {
        d = d.at(s.file, s.span, format!("this one is {}", other.display()));
    }
    if let Some(l) = site_label(
        first.prov.nearest_site(),
        &format!("the value that arrived first is {}", first.display()),
    ) {
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
        path_in(FileId(0), rel, at)
    }

    /// 別ファイル（＝別パッケージ）で宣言されたパス。
    fn path_in(file: FileId, rel: &str, at: u32) -> Value {
        Value {
            ty: Type::Path,
            data: Data::Path(crate::value::PathValue {
                base: crate::value::PathBase::Package,
                rel: rel.into(),
            }),
            prov: Prov::at(Origin::Call("dir".into()), Site::new(file, Span::new(at, at + 3))),
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
    fn union_drops_duplicates_and_keeps_arrival_order() {
        let def = lookup(Block::Public, "includes").unwrap();
        let a = Value::list(Type::Path, vec![path("include", 0), path("src", 10)], Prov::none());
        let b = Value::list(Type::Path, vec![path("src", 20), path("gen", 30)], Prov::none());
        let mut diags = Vec::new();
        let merged = merge_values(&def, &[a, b], &mut diags);
        assert!(diags.is_empty());
        let items = merged.as_list().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].display(), "include");
        assert_eq!(items[1].display(), "src");
        assert_eq!(items[2].display(), "gen");
    }

    #[test]
    fn merging_unwraps_a_list_nested_in_a_list() {
        // 列の要素に `match` を書くと、具体化した結果は列の中の列になる。
        // 1段しか解かないと、その値は併合の結果に残ったまま下流で読み飛ばされる。
        let def = lookup(Block::Private, "flags").unwrap();
        let inner = Value::list(
            Type::Str,
            vec![Value::str("-O0", Prov::none()), Value::str("-g", Prov::none())],
            Prov::none(),
        );
        let outer =
            Value::list(Type::Str, vec![Value::str("-Wall", Prov::none()), inner], Prov::none());
        let mut diags = Vec::new();
        let merged = merge_values(&def, &[outer], &mut diags);
        let items: Vec<String> = merged.as_list().unwrap().iter().map(|v| v.display()).collect();
        assert_eq!(items, ["\"-Wall\"", "\"-O0\"", "\"-g\""], "{items:?}");
    }

    #[test]
    fn union_keeps_the_same_relative_path_from_different_packages() {
        // パスの基点は値ではなく宣言位置が持つ。どのパッケージも公開ヘッダを
        // `include/` に置くのが慣習であるため、データだけで重複を判定すると
        // 依存が2段を超えた途端に別パッケージの include が消える。
        let def = lookup(Block::Public, "includes").unwrap();
        let a = Value::list(Type::Path, vec![path_in(FileId(1), "include", 0)], Prov::none());
        let b = Value::list(Type::Path, vec![path_in(FileId(2), "include", 0)], Prov::none());
        // 同じファイル内の重複は今までどおり落ちる。
        let c = Value::list(Type::Path, vec![path_in(FileId(2), "include", 40)], Prov::none());
        let mut diags = Vec::new();
        let merged = merge_values(&def, &[a, b, c], &mut diags);
        assert!(diags.is_empty());
        let items = merged.as_list().unwrap();
        assert_eq!(items.len(), 2, "{:?}", items.iter().map(|i| i.display()).collect::<Vec<_>>());
    }

    #[test]
    fn append_keeps_duplicates_and_order() {
        let def = lookup(Block::Private, "flags").unwrap();
        let f = |s: &str, at: u32| Value::str(s, Prov::at(Origin::Literal, site(at)));
        let a = Value::list(Type::Str, vec![f("-O2", 0), f("-g", 5)], Prov::none());
        let b = Value::list(Type::Str, vec![f("-O2", 10)], Prov::none());
        let mut diags = Vec::new();
        let merged = merge_values(&def, &[a, b], &mut diags);
        assert_eq!(merged.as_list().unwrap().len(), 3);
    }

    #[test]
    fn error_on_conflict_fails_on_differing_values() {
        let def = lookup(Block::Private, "defines").unwrap();
        let mut diags = Vec::new();
        merge_values(&def, &[map_of(&[("A", 1)], 0), map_of(&[("A", 1)], 9)], &mut diags);
        assert!(diags.is_empty(), "identical values must not conflict");
        merge_values(&def, &[map_of(&[("A", 1)], 0), map_of(&[("A", 2)], 9)], &mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "merge-conflict");
    }

    #[test]
    fn must_equal_fails_on_mismatch() {
        let def = lookup(Block::Public, "abi").unwrap();
        let label = |s: &str, at: u32| Value {
            ty: Type::AbiLabel,
            data: Data::Str(s.into()),
            prov: Prov::at(Origin::Literal, site(at)),
        };
        let mut diags = Vec::new();
        merge_values(&def, &[label("gnu11", 0), label("gnu11", 9)], &mut diags);
        assert!(diags.is_empty());
        merge_values(&def, &[label("gnu11", 0), label("cxx11abi0", 9)], &mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "abi-mismatch");
    }

    #[test]
    fn the_c_abi_label_matches_any_language_label() {
        // ADR-0019。C のライブラリと C++ の利用者は、正しく書けば違う札になる。
        let def = lookup(Block::Public, "abi").unwrap();
        let label = |s: &str, at: u32| Value {
            ty: Type::AbiLabel,
            data: Data::Str(s.into()),
            prov: Prov::at(Origin::Literal, site(at)),
        };
        let mut diags = Vec::new();
        let merged = merge_values(&def, &[label("gnu++17", 0), label(C_ABI, 9)], &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        // 制約を足さないだけで、消しはしない。利用者自身の札が残る。
        assert_eq!(merged.as_str(), Some("gnu++17"));

        // 順序に依らない。
        let merged = merge_values(&def, &[label(C_ABI, 0), label("gnu++17", 9)], &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(merged.as_str(), Some("gnu++17"));

        // 全てが `c` なら `c`。
        let merged = merge_values(&def, &[label(C_ABI, 0), label(C_ABI, 9)], &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(merged.as_str(), Some(C_ABI));
    }

    #[test]
    fn a_real_mismatch_still_fails_across_a_c_surface() {
        // `c` は突き合わせを1件緩めるだけで、他の札同士は依然として一致を要する。
        let def = lookup(Block::Public, "abi").unwrap();
        let label = |s: &str, at: u32| Value {
            ty: Type::AbiLabel,
            data: Data::Str(s.into()),
            prov: Prov::at(Origin::Literal, site(at)),
        };
        let mut diags = Vec::new();
        merge_values(&def, &[label("gnu11", 0), label(C_ABI, 9), label("gnu++17", 18)], &mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "abi-mismatch");
    }

    #[test]
    fn must_equal_on_another_property_is_untouched_by_the_c_label() {
        // 除外は ABI 札の語彙の性質である。`must_equal` そのものの性質にすると、
        // 別のプロパティで `"c"` という値が黙って一致扱いになる。
        let def = PropDef {
            name: "thing",
            ty: Type::Str,
            merge: Merge::MustEqual,
            doc: "",
            domain: None,
        };
        let v = |s: &str, at: u32| Value {
            ty: Type::Str,
            data: Data::Str(s.into()),
            prov: Prov::at(Origin::Literal, site(at)),
        };
        let mut diags = Vec::new();
        merge_values(&def, &[v("c", 0), v("d", 9)], &mut diags);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn lists_known_property_names() {
        assert!(prop_names(Block::Public).contains(&"includes"));
        assert!(prop_names(Block::Root).contains(&"sources"));
        assert!(!prop_names(Block::Root).contains(&"includes"));
    }
}
