//! 最小の JSON 読み取り。
//!
//! 対象は言語サーバが受け取る要求である。外部が生成した本文を読むため、
//! どのバイト列に対しても panic せず、読めなければ `None` を返す。
//!
//! 自前に持つ理由は
//! [ADR-0007](../../../docs/adr/0007-implementation-language.md)。
//!
//! ## 実装していないもの
//!
//! - 数値は `f64` として読み、整数は `as_i64` で取り出す。LSP の数値は
//!   行・桁・要求 ID であり、いずれも倍精度で表せる範囲に収まる
//! - 重複キーは後勝ち。JSON の仕様が定めていないため、実装の都合で決める
//! - 深さの上限を設ける。外部からの入力で再帰が尽きるのを防ぐ

use std::collections::BTreeMap;

/// 入れ子の上限。LSP の要求は数段で足りる。
///
/// 上限を設けないと、開き括弧を並べた本文で再帰が尽きる。読めない本文は
/// `None` になるだけであり、超えた場合の扱いは他の構文誤りと変わらない。
const MAX_DEPTH: usize = 64;

#[derive(Clone, PartialEq, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    /// オブジェクトの要素。対象がオブジェクトでなければ `None`。
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(m) => m.get(key),
            _ => None,
        }
    }

    /// ドットで区切った経路。`params.textDocument.uri` のように読む。
    pub fn path(&self, path: &str) -> Option<&Json> {
        path.split('.').try_fold(self, |cur, key| cur.get(key))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// 整数として読む。小数や範囲外は `None`。
    pub fn as_i64(&self) -> Option<i64> {
        let n = self.as_f64()?;
        if n.fract() != 0.0 || n < i64::MIN as f64 || n > i64::MAX as f64 {
            return None;
        }
        Some(n as i64)
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(v) => Some(v),
            _ => None,
        }
    }
}

/// 本文全体を読む。余りがある場合も読めなかったものとして扱う。
pub fn parse(text: &str) -> Option<Json> {
    let mut r = Reader { b: text.as_bytes(), i: 0, depth: 0 };
    let v = r.value()?;
    r.skip_ws();
    if r.i != r.b.len() {
        return None;
    }
    Some(v)
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Option<()> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.i += 1;
            Some(())
        } else {
            None
        }
    }

    fn literal(&mut self, word: &str) -> Option<()> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Some(())
        } else {
            None
        }
    }

    fn value(&mut self) -> Option<Json> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        match self.peek()? {
            b'n' => self.literal("null").map(|_| Json::Null),
            b't' => self.literal("true").map(|_| Json::Bool(true)),
            b'f' => self.literal("false").map(|_| Json::Bool(false)),
            b'"' => self.string().map(Json::Str),
            b'[' => self.array(),
            b'{' => self.object(),
            _ => self.number(),
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.expect(b'[')?;
        self.depth += 1;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            self.depth -= 1;
            return Some(Json::Array(out));
        }
        loop {
            out.push(self.value()?);
            self.skip_ws();
            match self.bump()? {
                b',' => continue,
                b']' => break,
                _ => return None,
            }
        }
        self.depth -= 1;
        Some(Json::Array(out))
    }

    fn object(&mut self) -> Option<Json> {
        self.expect(b'{')?;
        self.depth += 1;
        let mut out = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            self.depth -= 1;
            return Some(Json::Object(out));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.expect(b':')?;
            let value = self.value()?;
            out.insert(key, value);
            self.skip_ws();
            match self.bump()? {
                b',' => continue,
                b'}' => break,
                _ => return None,
            }
        }
        self.depth -= 1;
        Some(Json::Object(out))
    }

    fn string(&mut self) -> Option<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            match self.bump()? {
                b'"' => return Some(out),
                b'\\' => match self.bump()? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => out.push(self.unicode_escape()?),
                    _ => return None,
                },
                // 制御文字は生では現れない。
                c if c < 0x20 => return None,
                c => {
                    // UTF-8 の後続バイトはそのまま積み、最後に一度だけ検証する。
                    let start = self.i - 1;
                    let len = utf8_len(c)?;
                    self.i = start + len;
                    let bytes = self.b.get(start..self.i)?;
                    out.push_str(std::str::from_utf8(bytes).ok()?);
                }
            }
        }
    }

    /// `\uXXXX`。代用対を組み、単独の代用符号は置換文字にする。
    fn unicode_escape(&mut self) -> Option<char> {
        let high = self.hex4()?;
        if !(0xD800..0xDC00).contains(&high) {
            return Some(char::from_u32(high).unwrap_or('\u{fffd}'));
        }
        // 上位代用の後に `\uXXXX` が続かない本文は読める。文字だけを落とす。
        let saved = self.i;
        let paired = (|| {
            self.expect(b'\\')?;
            self.expect(b'u')?;
            let low = self.hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return None;
            }
            char::from_u32(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00))
        })();
        match paired {
            Some(c) => Some(c),
            None => {
                self.i = saved;
                Some('\u{fffd}')
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let s = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        let v = u32::from_str_radix(s, 16).ok()?;
        self.i += 4;
        Some(v)
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.i += 1;
        }
        let s = std::str::from_utf8(self.b.get(start..self.i)?).ok()?;
        s.parse::<f64>().ok().filter(|n| n.is_finite()).map(Json::Num)
    }
}

/// UTF-8 の先頭バイトから続く長さ。不正な先頭バイトは `None`。
fn utf8_len(first: u8) -> Option<usize> {
    match first {
        0x00..=0x7f => Some(1),
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shape_of_a_request() {
        let v = parse(
            r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover",
                "params":{"textDocument":{"uri":"file:///a/dowel.build"},
                          "position":{"line":3,"character":7}}}"#,
        )
        .expect("the request should parse");
        assert_eq!(v.path("method").and_then(|m| m.as_str()), Some("textDocument/hover"));
        assert_eq!(v.path("id").and_then(|i| i.as_i64()), Some(1));
        assert_eq!(
            v.path("params.textDocument.uri").and_then(|u| u.as_str()),
            Some("file:///a/dowel.build")
        );
        assert_eq!(v.path("params.position.line").and_then(|l| l.as_i64()), Some(3));
        assert_eq!(v.path("params.nothing.here"), None);
    }

    #[test]
    fn reads_every_kind_of_value() {
        let v = parse(r#"[null,true,false,0,-1.5,1e3,"s",[],{}]"#).unwrap();
        let items = v.as_array().unwrap();
        assert_eq!(items[0], Json::Null);
        assert_eq!(items[1].as_bool(), Some(true));
        assert_eq!(items[2].as_bool(), Some(false));
        assert_eq!(items[3].as_i64(), Some(0));
        assert_eq!(items[4].as_f64(), Some(-1.5));
        // 指数表記も整数として取り出せる。
        assert_eq!(items[5].as_i64(), Some(1000));
        assert_eq!(items[6].as_str(), Some("s"));
        assert_eq!(items[7].as_array().unwrap().len(), 0);
        assert_eq!(items[8].get("nothing"), None);
    }

    #[test]
    fn a_fractional_number_is_not_an_integer() {
        // 行や桁として読む値であり、小数を黙って切り捨てない。
        assert_eq!(parse("1.5").unwrap().as_i64(), None);
    }

    #[test]
    fn round_trips_with_the_writer() {
        // 書き出した本文を読み戻せること。エスケープの扱いが両側で揃う。
        let mut w = crate::json::JsonWriter::new();
        w.begin_object();
        w.field_str("message", "改行\nと \"引用\" と \\ と \u{7f}");
        w.end_object();
        let text = w.finish();
        let back = parse(&text).expect("the writer produced something unreadable");
        assert_eq!(
            back.path("message").and_then(|m| m.as_str()),
            Some("改行\nと \"引用\" と \\ と \u{7f}")
        );
    }

    #[test]
    fn escapes_are_decoded() {
        let v = parse(r#""a\u00e9\u3042\ud83d\ude00b\/\t""#).unwrap();
        assert_eq!(v.as_str(), Some("aéあ😀b/\t"));
    }

    #[test]
    fn a_lone_surrogate_becomes_the_replacement_character() {
        // 落とすのは文字だけであり、本文全体は読めるままにする。
        assert_eq!(parse(r#""\ud800""#).unwrap().as_str(), Some("\u{fffd}"));
        assert_eq!(parse(r#""\ud800x""#).unwrap().as_str(), Some("\u{fffd}x"));
    }

    #[test]
    fn trailing_bytes_do_not_parse() {
        // 読めたところまでを使わない。形式が合っていない証拠である。
        assert_eq!(parse("{} {}"), None);
        assert_eq!(parse("1 2"), None);
    }

    #[test]
    fn malformed_input_returns_none_and_does_not_panic() {
        for text in [
            "",
            "{",
            "}",
            "[",
            "]",
            "{\"a\"}",
            "{\"a\":}",
            "{,}",
            "[1,]",
            "[,1]",
            "\"",
            "\"\\",
            "\"\\q\"",
            "\"\\u00\"",
            "tru",
            "nul",
            "-",
            "1.2.3",
            "{\"a\":1,}",
            "\u{1}",
        ] {
            assert_eq!(parse(text), None, "`{text}` should not parse");
        }
    }

    #[test]
    fn deep_nesting_is_refused_instead_of_exhausting_the_stack() {
        let deep = "[".repeat(MAX_DEPTH + 10) + &"]".repeat(MAX_DEPTH + 10);
        assert_eq!(parse(&deep), None);
        // 上限までは読める。
        let ok = "[".repeat(MAX_DEPTH - 1) + &"]".repeat(MAX_DEPTH - 1);
        assert!(parse(&ok).is_some());
    }

    #[test]
    fn arbitrary_bytes_do_not_panic() {
        // 外部が生成した本文を読む。どの入力でも落ちないこと。
        for seed in 0u32..512 {
            let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
            let mut s = String::new();
            for _ in 0..(seed % 48) {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                // JSON に現れる文字の中から選ぶ。構文の境界を踏みやすくする。
                let alphabet = b"{}[]\":,0123456789.eE+-truefalsnl \\/\tu";
                s.push(alphabet[(x >> 16) as usize % alphabet.len()] as char);
            }
            let _ = parse(&s);
        }
    }

    #[test]
    fn a_duplicate_key_takes_the_last_value() {
        let v = parse(r#"{"a":1,"a":2}"#).unwrap();
        assert_eq!(v.get("a").and_then(|x| x.as_i64()), Some(2));
    }
}
