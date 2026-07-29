//! 最小の JSON。
//!
//! 書き出しは診断とグラフの機械可読出力が、読み取りは言語サーバが受け取る
//! 要求が使う。自前に持つ理由は
//! [ADR-0007](../../../docs/adr/0007-implementation-language.md)。

mod read;

pub use read::{parse, Json};

use std::fmt::Write as _;

/// JSON を組み立てるためのバッファ。
///
/// 文字列連結ではなくこの型を経由させるのは、エスケープ漏れを型で防ぐためである。
/// 診断の JSON 出力（docs/30-devexp.md 4節）は機械が消費するため、
/// 壊れた JSON を出すことは人間向け出力の乱れより重い。
#[derive(Default)]
pub struct JsonWriter {
    buf: String,
    /// 各ネストレベルで要素を1つ以上書いたか。区切りのカンマ挿入に使う。
    stack: Vec<bool>,
    /// 直前に `key()` を書いた。次の値は区切りを挿入しない。
    pending_key: bool,
    pretty: bool,
}

impl JsonWriter {
    pub fn new() -> JsonWriter {
        JsonWriter::default()
    }

    pub fn pretty() -> JsonWriter {
        JsonWriter { pretty: true, ..JsonWriter::default() }
    }

    pub fn finish(self) -> String {
        debug_assert!(self.stack.is_empty(), "unclosed JSON nesting");
        debug_assert!(!self.pending_key, "key written without a value");
        self.buf
    }

    fn indent(&mut self) {
        self.buf.push('\n');
        for _ in 0..self.stack.len() {
            self.buf.push_str("  ");
        }
    }

    /// 値を書く直前の区切り処理。キー直後の値には区切りを入れない。
    fn before_value(&mut self) {
        if self.pending_key {
            self.pending_key = false;
            return;
        }
        if let Some(has_item) = self.stack.last_mut() {
            let first = !*has_item;
            *has_item = true;
            if !first {
                self.buf.push(',');
            }
            if self.pretty {
                self.indent();
            }
        }
    }

    fn close(&mut self, ch: char) {
        let had_item = self.stack.pop().unwrap_or(false);
        if self.pretty && had_item {
            self.indent();
        }
        self.buf.push(ch);
    }

    pub fn begin_object(&mut self) -> &mut Self {
        self.before_value();
        self.buf.push('{');
        self.stack.push(false);
        self
    }

    pub fn end_object(&mut self) -> &mut Self {
        self.close('}');
        self
    }

    pub fn begin_array(&mut self) -> &mut Self {
        self.before_value();
        self.buf.push('[');
        self.stack.push(false);
        self
    }

    pub fn end_array(&mut self) -> &mut Self {
        self.close(']');
        self
    }

    /// オブジェクトのキー。直後に値をちょうど1つ書くこと。
    pub fn key(&mut self, k: &str) -> &mut Self {
        self.before_value();
        escape_into(&mut self.buf, k);
        self.buf.push(':');
        if self.pretty {
            self.buf.push(' ');
        }
        self.pending_key = true;
        self
    }

    pub fn str(&mut self, v: &str) -> &mut Self {
        self.before_value();
        escape_into(&mut self.buf, v);
        self
    }

    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.before_value();
        let _ = write!(self.buf, "{v}");
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.before_value();
        let _ = write!(self.buf, "{v}");
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.before_value();
        self.buf.push_str(if v { "true" } else { "false" });
        self
    }

    pub fn null(&mut self) -> &mut Self {
        self.before_value();
        self.buf.push_str("null");
        self
    }

    // キーと値の対を書く短縮形。呼び出し側の記述量が診断の網羅性に効くため用意する。
    pub fn field_str(&mut self, k: &str, v: &str) -> &mut Self {
        self.key(k).str(v)
    }

    pub fn field_u64(&mut self, k: &str, v: u64) -> &mut Self {
        self.key(k).u64(v)
    }

    pub fn field_bool(&mut self, k: &str, v: bool) -> &mut Self {
        self.key(k).bool(v)
    }

    pub fn field_strs<'a>(&mut self, k: &str, vs: impl IntoIterator<Item = &'a str>) -> &mut Self {
        self.key(k).begin_array();
        for v in vs {
            self.str(v);
        }
        self.end_array()
    }
}

fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// 単発の文字列エスケープ。
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    escape_into(&mut out, s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_and_separators() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_str("name", "foo");
        w.key("items").begin_array();
        w.i64(1);
        w.i64(2);
        w.end_array();
        w.key("nested").begin_object();
        w.field_bool("ok", true);
        w.end_object();
        w.end_object();
        assert_eq!(w.finish(), r#"{"name":"foo","items":[1,2],"nested":{"ok":true}}"#);
    }

    #[test]
    fn escapes_control_characters_and_quotes() {
        let mut w = JsonWriter::new();
        w.str("a\"b\\c\nd\u{1}e");
        assert_eq!(w.finish(), r#""a\"b\\c\nd\u0001e""#);
    }

    #[test]
    fn empty_array_and_object() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.key("a").begin_array();
        w.end_array();
        w.key("b").begin_object();
        w.end_object();
        w.end_object();
        assert_eq!(w.finish(), r#"{"a":[],"b":{}}"#);
    }

    #[test]
    fn pretty_inserts_newlines_and_indentation() {
        let mut w = JsonWriter::pretty();
        w.begin_object();
        w.field_str("k", "v");
        w.key("xs").begin_array();
        w.i64(1);
        w.end_array();
        w.end_object();
        assert_eq!(w.finish(), "{\n  \"k\": \"v\",\n  \"xs\": [\n    1\n  ]\n}");
    }
}
