//! 直列化の検査。
//!
//! 復元した値の同一性は、再度書き出したバイト列が一致することで確かめる。
//! `Document` は `PartialEq` を持たず、比較のためだけに導出すると
//! 「どの差異を無視するか」を型に持たせることになる。
//!
//! 併せて、壊れた入力に対して panic せず `None` を返すことを確かめる。
//! ストアの値は外部から書き換えられうるため、この性質が要る。

use super::*;
use crate::value::{Origin, Prov, Site, Type, Value};
use dowel_support::{FileId, Span};

/// 実際のマニフェストを評価して得た文書。
fn evaluate(src: &str) -> Document {
    let file = FileId(42);
    let parsed = dowel_syntax::parse(src, file);
    let (doc, _) = crate::eval(&parsed.root, src, file);
    doc
}

/// 書き出し → 復元 → 書き出しが一致すること。
fn round_trips(doc: &Document) {
    let first = encode_document(doc);
    let back = decode_document(&first).expect("the document should decode");
    let second = encode_document(&back);
    assert_eq!(first, second, "the value changed across a round trip");
}

#[test]
fn an_empty_document_round_trips() {
    round_trips(&evaluate(""));
}

#[test]
fn a_realistic_manifest_round_trips() {
    let doc = evaluate(
        r#"
[lib.foo]
sources = glob("src/**.c")

[lib.foo.public]
includes = [dir("include")]
defines  = { FOO_API = 1, NAME = "foo", ON = true }

[lib.foo.private]
includes = [dir("src")]
flags    = ["-Wall", "-Wextra"]
deps     = [dep("bar"), target("baz")]
abi      = "x86_64-gnu"
"#,
    );
    assert!(!doc.tables.is_empty());
    round_trips(&doc);
}

#[test]
fn match_and_postfix_when_round_trip() {
    // 構成で分岐する値は具体化前の形のまま格納する。
    let doc = evaluate(
        r#"
[bin.app]
sources = glob("src/*.c")

[bin.app.private]
flags = match cfg.opt {
    debug   => ["-O0", "-g"],
    release => ["-O2"],
    _       => [],
}
link_flags = ["-lz"] when feature.zlib
"#,
    );
    round_trips(&doc);
}

#[test]
fn a_document_with_errors_round_trips() {
    // 誤りを含む文書も格納の対象である。診断は別に持つが、
    // `Data::Error` を含む値そのものは復元できなければならない。
    round_trips(&evaluate("[bin.app]\nsources = nosuchfn(\"x\")\n"));
}

#[test]
fn the_file_identifier_survives() {
    let doc = evaluate("[bin.app]\nsources = glob(\"*.c\")\n");
    let back = decode_document(&encode_document(&doc)).unwrap();
    assert_eq!(back.file, FileId(42));
    let site = back.tables[0].site;
    assert_eq!(site.file, FileId(42));
}

#[test]
fn the_provenance_chain_survives_in_order() {
    // 来歴は自分から根への順で読める。`dowel why` の出力がこの順である。
    let prov = Prov::at(Origin::Literal, Site { file: FileId(7), span: Span::new(1, 2) })
        .then(Origin::Call("dir".into()), None)
        .then(Origin::Merged { prop: "includes".into(), rule: "union" }, None);
    let doc = Document {
        file: FileId(7),
        cfg_refs: Vec::new(),
        tables: vec![Table {
            path: vec!["lib".into(), "a".into()],
            path_spans: vec![Span::new(1, 4), Span::new(5, 6)],
            array: false,
            site: Site { file: FileId(7), span: Span::new(0, 5) },
            entries: vec![Entry {
                key: vec!["includes".into()],
                key_spans: vec![Span::new(6, 9)],
                site: Site { file: FileId(7), span: Span::new(6, 9) },
                value: Value { ty: Type::Path, data: Data::Glob("*.c".into()), prov },
            }],
        }],
    };
    round_trips(&doc);

    let back = decode_document(&encode_document(&doc)).unwrap();
    let chain = back.tables[0].entries[0].value.prov.chain();
    assert_eq!(chain.len(), 3);
    assert!(matches!(chain[0].0, Origin::Merged { .. }));
    assert!(matches!(chain[1].0, Origin::Call(_)));
    assert!(matches!(chain[2].0, Origin::Literal));
    // 位置は最も根に近い段が持っていた。
    assert_eq!(chain[2].1, Some(Site { file: FileId(7), span: Span::new(1, 2) }));
}

#[test]
fn the_configuration_references_survive() {
    // 機能名の検証は復元した文書に対しても働かなければならない。
    // 参照の一覧を落とすと、ストア経由の実行だけが綴り誤りを見逃す。
    let doc = evaluate(
        "[bin.app]\nsources = glob(\"*.c\")\n\n[bin.app.private]\nflags = [\"-O2\" when feature.fast]\n",
    );
    assert_eq!(doc.cfg_refs.len(), 1);
    let back = decode_document(&encode_document(&doc)).unwrap();
    assert_eq!(back.cfg_refs.len(), 1);
    assert_eq!(back.cfg_refs[0].key.display(), "feature.fast");
    assert_eq!(back.cfg_refs[0].site, doc.cfg_refs[0].site);
    round_trips(&doc);
}

#[test]
fn the_segment_spans_survive() {
    // 修正提案は段ごとの位置を使う。復元した文書から提案を出せなければ、
    // ストア経由の実行だけが範囲の誤った提案を出すことになる。
    let src = "[lib.foo.public]\nincludes = [dir(\"include\")]\n";
    let back = decode_document(&encode_document(&evaluate(src))).unwrap();
    let table = &back.tables[0];
    assert_eq!(table.path, ["lib", "foo", "public"]);
    let text = |s: dowel_support::Span| &src[s.range()];
    assert_eq!(
        table.path_spans.iter().map(|&s| text(s)).collect::<Vec<_>>(),
        ["lib", "foo", "public"]
    );
    let entry = &table.entries[0];
    assert_eq!(entry.key_spans.iter().map(|&s| text(s)).collect::<Vec<_>>(), ["includes"]);
}

#[test]
fn a_merge_rule_that_is_not_known_becomes_unknown() {
    assert_eq!(merge_rule("union"), "union");
    assert_eq!(merge_rule("must_equal"), "must_equal");
    assert_eq!(merge_rule("nonsense"), "unknown");
}

#[test]
fn a_wrong_version_does_not_decode() {
    let doc = evaluate("[bin.app]\nsources = glob(\"*.c\")\n");
    let mut bytes = encode_document(&doc);
    bytes[0] = VERSION.wrapping_add(1);
    assert!(decode_document(&bytes).is_none());
}

#[test]
fn truncated_input_does_not_decode_and_does_not_panic() {
    let doc = evaluate(
        "[lib.foo]\nsources = glob(\"src/*.c\")\n\n[lib.foo.public]\nincludes = [dir(\"include\")]\n",
    );
    let bytes = encode_document(&doc);
    assert!(bytes.len() > 32, "the fixture should be large enough to truncate");
    // 全ての切り詰め位置で、panic せずに `None` を返すこと。
    for cut in 0..bytes.len() {
        assert!(decode_document(&bytes[..cut]).is_none(), "decoded a prefix of length {cut}");
    }
}

#[test]
fn trailing_bytes_do_not_decode() {
    // 余りがある場合、読めたところまでを使わない。形式が合っていない証拠である。
    let doc = evaluate("[bin.app]\nsources = glob(\"*.c\")\n");
    let mut bytes = encode_document(&doc);
    bytes.push(0);
    assert!(decode_document(&bytes).is_none());
}

#[test]
fn arbitrary_bytes_do_not_panic() {
    // ストアの値は外部から書き換えられうる。どのバイト列でも落ちないこと。
    assert!(decode_document(&[]).is_none());
    assert!(decode_document(&[VERSION]).is_none());
    for seed in 0u32..512 {
        // 決まった手順で作る疑似乱数。実行ごとに同じ入力を試す。
        let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
        let mut bytes = vec![VERSION];
        for _ in 0..(seed % 64) {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            bytes.push((x >> 16) as u8);
        }
        let _ = decode_document(&bytes);
    }
}

#[test]
fn every_type_variant_round_trips() {
    // 型は再帰的である。入れ子にした形も含めて確かめる。
    let types = [
        Type::Str,
        Type::Int,
        Type::Bool,
        Type::Path,
        Type::DepRef,
        Type::TargetRef,
        Type::AbiLabel,
        Type::Val,
        Type::Unknown,
        Type::List(Box::new(Type::Path)),
        Type::Set(Box::new(Type::Str)),
        Type::Map(Box::new(Type::Val)),
        Type::Cfg(Box::new(Type::List(Box::new(Type::Str)))),
    ];
    for ty in types {
        let mut w = W(Vec::new());
        w.ty(&ty);
        let mut r = R { b: &w.0, i: 0 };
        let back = r.ty().expect("the type should decode");
        assert_eq!(back.display(), ty.display());
        assert_eq!(r.i, r.b.len(), "bytes were left over for {}", ty.display());
    }
}
