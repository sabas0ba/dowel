//! パーサの頑健性。
//!
//! 誤り耐性は「よくある書き間違い」だけでなく、任意の壊れた入力に対して
//! 成立していなければならない。言語サーバは編集途中のバッファを常に食わされるため、
//! 中途半端な入力こそが定常状態である。
//!
//! ここでは実マニフェストを機械的に壊し、次の2点を検査する。
//!
//! - パニックしない
//! - CST がロスレスである（連結すると入力に戻る）

use dowel_support::FileId;
use dowel_syntax::parse;

const SAMPLE: &str = r#"
# libfoo のターゲット定義
[lib.foo]
sources = glob("src/**.c")

[lib.foo.public]
includes = [dir("include")]
deps     = [dep("bar"), dep("mylib")]

[lib.foo.private]
includes = [dir("src")]
defines  = { FOO_BUILDING = 1, LEVEL = 3 }
deps     = [dep("zlib") when feature.zlib]
flags    = match cfg.opt {
    debug   => ["-O0", "-g3"],
    release => ["-O2", "-DNDEBUG"],
}

[test.unit]
sources = glob("tests/*.c")
deps    = [target("foo")]
"#;

fn check(src: &str) {
    let parsed = parse(src, FileId(0));
    assert_eq!(parsed.root.text(src), src, "ロスレス性が崩れた入力:\n{src}");
}

#[test]
fn 全ての接頭辞で破綻しない() {
    // 編集途中のバッファはほぼ常に「途中で切れた入力」である。
    for end in 0..=SAMPLE.len() {
        if !SAMPLE.is_char_boundary(end) {
            continue;
        }
        check(&SAMPLE[..end]);
    }
}

#[test]
fn 一文字削除しても破綻しない() {
    let chars: Vec<char> = SAMPLE.chars().collect();
    for i in 0..chars.len() {
        let mut s: String = chars[..i].iter().collect();
        s.extend(&chars[i + 1..]);
        check(&s);
    }
}

#[test]
fn 区切り記号を差し込んでも破綻しない() {
    // 括弧の不整合が最も復帰の難しい入力になる。
    let noises = ["[", "]", "{", "}", "(", ")", "\"", "'", ",", "=", "=>", "@", "match", "when"];
    let positions = [0, 37, 80, 140, 220, SAMPLE.len()];
    for noise in noises {
        for &pos in &positions {
            if pos > SAMPLE.len() || !SAMPLE.is_char_boundary(pos) {
                continue;
            }
            let mut s = String::from(&SAMPLE[..pos]);
            s.push_str(noise);
            s.push_str(&SAMPLE[pos..]);
            check(&s);
        }
    }
}

#[test]
fn 深い入れ子で停止する() {
    // 再帰下降であるため、極端な入れ子は原理的にスタックを消費する。
    // 現実的な深さでは問題にならないことを確認しておく。
    let src = format!("a = {}{}", "[".repeat(200), "]".repeat(200));
    check(&src);
}

#[test]
fn 正しい入力に診断は出ない() {
    let parsed = parse(SAMPLE, FileId(0));
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    assert!(!parsed.root.has_error());
}
