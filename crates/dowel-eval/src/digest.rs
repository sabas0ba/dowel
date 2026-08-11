//! 位置を含まない値の要約。
//!
//! 増分エンジンの early cutoff（docs/20-architecture.md 3節）は
//! 「再評価結果が前回と同一なら依存側を無効化しない」機構である。
//! 値そのものを指紋にすると、コメントの追加で全てのスパンがずれるため、
//! 意味の変わらない編集でも常に「変わった」と判定される。
//!
//! ここで求める要約は型と本体だけを畳み込む。スパンは含めない。
//!
//! ## ファイル識別子は含める
//!
//! パスは「パッケージルートからの相対」で表され、基点は値ではなく宣言位置が持つ
//! （docs/10-manifest.md 3節）。同じ `dir("include")` でも宣言したファイルが違えば
//! 指す先は別のディレクトリである。したがって最寄りの宣言位置の `FileId` は
//! 要約に含める。含めないと、別パッケージの同名ディレクトリが同一と見なされる。
//!
//! `FileId` は正規化したパスのハッシュであり、編集では変わらない
//! （[ADR-0009](../../../docs/adr/0009-file-identity.md)）。

use crate::value::{Data, Pred, Type, Value};
use std::hash::{Hash, Hasher};

/// 値の要約。スパンを含まないため、コメントや空白の編集では変わらない。
pub fn value_digest(v: &Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    write_value(&mut h, v);
    h.finish()
}

/// 複数の値をまとめた要約。名前つきの写像に使う。
pub fn props_digest<'a>(props: impl IntoIterator<Item = (&'a str, &'a Value)>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (name, v) in props {
        name.hash(&mut h);
        write_value(&mut h, v);
    }
    h.finish()
}

fn write_value<H: Hasher>(h: &mut H, v: &Value) {
    write_type(h, &v.ty);
    // 宣言位置のうちファイルだけを含める。スパンは編集で動く。
    v.prov.nearest_site().map(|s| s.file.0).hash(h);
    write_data(h, &v.data);
}

fn write_type<H: Hasher>(h: &mut H, t: &Type) {
    // 表示形は型に対して単射である。再帰の記述を1箇所に閉じるため利用する。
    t.display().hash(h);
}

fn write_data<H: Hasher>(h: &mut H, d: &Data) {
    // 判別子は明示的に振る。同じ本体を持つ別の種類を同一と見なさないため。
    match d {
        Data::Str(s) => (0u8, s).hash(h),
        Data::Int(i) => (1u8, i).hash(h),
        Data::Bool(b) => (2u8, b).hash(h),
        Data::Path(p) => (3u8, p.base as u8, &p.rel).hash(h),
        Data::Glob(g) => (4u8, g).hash(h),
        Data::Dep(n) => (5u8, n).hash(h),
        Data::Target(n) => (6u8, n).hash(h),
        Data::List(items) => {
            (7u8, items.len()).hash(h);
            for item in items {
                write_value(h, item);
            }
        }
        Data::Map(m) => {
            (8u8, m.len()).hash(h);
            for (k, v) in m {
                k.hash(h);
                write_value(h, v);
            }
        }
        Data::Match { scrutinee, arms } => {
            (9u8, scrutinee.display(), arms.len()).hash(h);
            for a in arms {
                a.pattern.display().hash(h);
                write_value(h, &a.value);
            }
        }
        Data::When { pred, inner } => {
            (10u8, pred_key(pred)).hash(h);
            write_value(h, inner);
        }
        Data::PkgRef(name) => (12u8, name).hash(h),
        Data::Error => 11u8.hash(h),
    }
}

fn pred_key(p: &Pred) -> String {
    match p {
        Pred::Flag(k) => k.display(),
        Pred::Eq(k, v) => format!("{}=={v:?}", k.display()),
        // 括弧を書く。`a and (b or c)` と `(a and b) or c` が同じ綴りに
        // 潰れると、片方への変更が早期打ち切りに吸われる
        Pred::Not(p) => format!("!({})", pred_key(p)),
        Pred::And(a, b) => format!("({}&&{})", pred_key(a), pred_key(b)),
        Pred::Or(a, b) => format!("({}||{})", pred_key(a), pred_key(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{Origin, PathBase, PathValue, Prov, Site};
    use dowel_support::{FileId, Span};

    fn at(file: u64, start: u32, data: Data) -> Value {
        Value {
            ty: Type::Path,
            data,
            prov: Prov::at(
                Origin::Literal,
                Site { file: FileId(file), span: Span::new(start, start + 3) },
            ),
        }
    }

    fn dir(rel: &str) -> Data {
        Data::Path(PathValue { base: PathBase::Package, rel: rel.to_string() })
    }

    #[test]
    fn moving_a_value_within_its_file_does_not_change_the_digest() {
        // コメントを1行足すとスパンは全て動く。意味は変わらない。
        let a = at(1, 10, dir("include"));
        let b = at(1, 99, dir("include"));
        assert_eq!(value_digest(&a), value_digest(&b));
    }

    #[test]
    fn the_declaring_file_is_part_of_the_digest() {
        // 同じ `dir("include")` でも、宣言したファイルが違えば別のディレクトリを指す。
        let a = at(1, 10, dir("include"));
        let b = at(2, 10, dir("include"));
        assert_ne!(value_digest(&a), value_digest(&b));
    }

    #[test]
    fn the_content_is_part_of_the_digest() {
        assert_ne!(value_digest(&at(1, 0, dir("include"))), value_digest(&at(1, 0, dir("src"))));
    }

    #[test]
    fn a_list_distinguishes_order() {
        let prov = Prov::none();
        let one = Value::list(
            Type::Str,
            vec![Value::str("a", prov.clone()), Value::str("b", prov.clone())],
            prov.clone(),
        );
        let two = Value::list(
            Type::Str,
            vec![Value::str("b", prov.clone()), Value::str("a", prov.clone())],
            prov,
        );
        assert_ne!(value_digest(&one), value_digest(&two));
    }

    #[test]
    fn different_kinds_with_the_same_body_differ() {
        let prov = Prov::none();
        let dep = Value { ty: Type::DepRef, data: Data::Dep("x".into()), prov: prov.clone() };
        let target = Value { ty: Type::TargetRef, data: Data::Target("x".into()), prov };
        assert_ne!(value_digest(&dep), value_digest(&target));
    }

    #[test]
    fn the_property_name_is_part_of_the_digest() {
        let prov = Prov::none();
        let v = Value::str("x", prov);
        assert_ne!(props_digest([("flags", &v)]), props_digest([("link_flags", &v)]));
    }
}
