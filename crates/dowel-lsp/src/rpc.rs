//! JSON-RPC の枠付けと本文の受け渡し。
//!
//! LSP は本文の前に `Content-Length: <バイト数>` の頭部を置き、空行で区切る。
//! 頭部は ASCII、本文は UTF-8 である。
//!
//! 頭部の読み取りは `read_line` ではなくバイト単位で行う。`read_line` は
//! 改行までを UTF-8 として解釈するが、頭部の直後に続く本文は同じ流れの上に
//! あり、緩衝に取り込まれると本文の先頭を失う。

use dowel_support::json::{Json, JsonWriter};
use std::io::{BufRead, Write};

/// 受け取った1件。
pub enum Message {
    /// 応答を要する。`id` を添えて返す
    Request { id: Json, method: String, params: Json },
    /// 応答を要さない
    Notification { method: String, params: Json },
}

impl Message {
    pub fn method(&self) -> &str {
        match self {
            Message::Request { method, .. } | Message::Notification { method, .. } => method,
        }
    }

    pub fn params(&self) -> &Json {
        match self {
            Message::Request { params, .. } | Message::Notification { params, .. } => params,
        }
    }
}

/// 本文を1件読む。流れの終端では `None`。
///
/// 読めない本文は捨てて次を読む。1件の不正で接続を落とさない。
pub fn read(input: &mut impl BufRead) -> std::io::Result<Option<Message>> {
    loop {
        let Some(len) = read_headers(input)? else { return Ok(None) };
        let mut body = vec![0u8; len];
        input.read_exact(&mut body)?;

        let Ok(text) = String::from_utf8(body) else {
            dowel_support::log_debug!("lsp: the body is not utf-8; skipping");
            continue;
        };
        let Some(value) = dowel_support::json::parse(&text) else {
            dowel_support::log_debug!("lsp: unreadable body; skipping");
            continue;
        };
        let Some(method) = value.path("method").and_then(|m| m.as_str()) else {
            // 応答（`result` や `error` を持つ）は要求しない限り届かない。
            dowel_support::log_debug!("lsp: a message without a method; skipping");
            continue;
        };
        let method = method.to_string();
        let params = value.get("params").cloned().unwrap_or(Json::Null);
        return Ok(Some(match value.get("id") {
            Some(id) => Message::Request { id: id.clone(), method, params },
            None => Message::Notification { method, params },
        }));
    }
}

/// 頭部を読み、本文のバイト数を返す。流れの終端では `None`。
fn read_headers(input: &mut impl BufRead) -> std::io::Result<Option<usize>> {
    let mut len: Option<usize> = None;
    loop {
        let line = match read_ascii_line(input)? {
            Some(l) => l,
            None => return Ok(None),
        };
        if line.is_empty() {
            return Ok(len);
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok();
        }
        // 他の頭部（`Content-Type`）は読み飛ばす。値は1つしか定義がない。
    }
}

/// `\r\n` までの1行。終端の `\r\n` は含まない。
fn read_ascii_line(input: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut out = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match input.read(&mut byte)? {
            0 if out.is_empty() => return Ok(None),
            0 => break,
            _ => {}
        }
        if byte[0] == b'\n' {
            break;
        }
        out.push(byte[0]);
    }
    if out.last() == Some(&b'\r') {
        out.pop();
    }
    Ok(Some(String::from_utf8_lossy(&out).into_owned()))
}

/// 本文を1件書く。
pub fn write(out: &mut impl Write, body: &str) -> std::io::Result<()> {
    write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    out.flush()
}

/// 応答の外枠。`result` の中身は呼び手が書く。
pub fn response(id: &Json, fill: impl FnOnce(&mut JsonWriter)) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_str("jsonrpc", "2.0");
    w.key("id");
    write_json(&mut w, id);
    w.key("result");
    fill(&mut w);
    w.end_object();
    w.finish()
}

/// 通知の外枠。
pub fn notification(method: &str, fill: impl FnOnce(&mut JsonWriter)) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_str("jsonrpc", "2.0");
    w.field_str("method", method);
    w.key("params");
    fill(&mut w);
    w.end_object();
    w.finish()
}

/// 誤りの応答。要求に応えられない場合に返す。
pub fn error(id: &Json, code: i64, message: &str) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_str("jsonrpc", "2.0");
    w.key("id");
    write_json(&mut w, id);
    w.key("error").begin_object();
    w.key("code").i64(code);
    w.field_str("message", message);
    w.end_object();
    w.end_object();
    w.finish()
}

/// 読み取った値をそのまま書き戻す。要求 ID は数値も文字列も取りうる。
fn write_json(w: &mut JsonWriter, v: &Json) {
    match v {
        Json::Null => {
            w.null();
        }
        Json::Bool(b) => {
            w.bool(*b);
        }
        Json::Num(n) => {
            // 要求 ID は整数で来る。小数は仕様上ありうるが、そのまま書き戻す
            // 手段が無いため文字列にする。応答の対応付けは維持される。
            match (n.fract() == 0.0 && n.abs() < 9e15).then_some(*n as i64) {
                Some(i) => w.i64(i),
                None => w.str(&n.to_string()),
            };
        }
        Json::Str(s) => {
            w.str(s);
        }
        Json::Array(items) => {
            w.begin_array();
            for item in items {
                write_json(w, item);
            }
            w.end_array();
        }
        Json::Object(m) => {
            w.begin_object();
            for (k, v) in m {
                w.key(k);
                write_json(w, v);
            }
            w.end_object();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn framed(bodies: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for b in bodies {
            write(&mut out, b).unwrap();
        }
        out
    }

    #[test]
    fn reads_consecutive_messages_from_one_stream() {
        // 頭部を行単位で読むと、緩衝が本文まで取り込んで2件目を失う。
        let bytes = framed(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        ]);
        let mut input = BufReader::new(&bytes[..]);

        let first = read(&mut input).unwrap().expect("the first message is missing");
        assert!(matches!(first, Message::Request { .. }));
        assert_eq!(first.method(), "initialize");

        let second = read(&mut input).unwrap().expect("the second message is missing");
        assert!(matches!(second, Message::Notification { .. }));
        assert_eq!(second.method(), "initialized");

        assert!(read(&mut input).unwrap().is_none(), "the stream should be exhausted");
    }

    #[test]
    fn a_body_that_does_not_parse_is_skipped() {
        // 1件の不正で接続を落とさない。エディタは送り続ける。
        let bytes = framed(&["{ not json", r#"{"method":"exit"}"#]);
        let mut input = BufReader::new(&bytes[..]);
        let m = read(&mut input).unwrap().expect("the following message was lost");
        assert_eq!(m.method(), "exit");
    }

    #[test]
    fn a_non_utf8_body_is_skipped() {
        let mut bytes = Vec::new();
        let bad = [0xffu8, 0xfe];
        write!(&mut bytes, "Content-Length: {}\r\n\r\n", bad.len()).unwrap();
        bytes.extend_from_slice(&bad);
        bytes.extend(framed(&[r#"{"method":"exit"}"#]));
        let mut input = BufReader::new(&bytes[..]);
        assert_eq!(read(&mut input).unwrap().unwrap().method(), "exit");
    }

    #[test]
    fn other_headers_are_ignored() {
        let body = r#"{"method":"exit"}"#;
        let mut bytes = Vec::new();
        write!(
            &mut bytes,
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut input = BufReader::new(&bytes[..]);
        assert_eq!(read(&mut input).unwrap().unwrap().method(), "exit");
    }

    #[test]
    fn the_request_id_is_written_back_unchanged() {
        // 応答の対応付けは ID の一致で行う。数値も文字列も来る。
        for (id, expected) in
            [(Json::Num(7.0), r#""id":7"#), (Json::Str("abc".into()), r#""id":"abc""#)]
        {
            let text = response(&id, |w| {
                w.null();
            });
            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn an_error_response_carries_the_code() {
        let text = error(&Json::Num(1.0), -32601, "method not found");
        let back = dowel_support::json::parse(&text).expect("the error is not valid json");
        assert_eq!(back.path("error.code").and_then(|c| c.as_i64()), Some(-32601));
        assert_eq!(back.path("error.message").and_then(|m| m.as_str()), Some("method not found"));
    }

    #[test]
    fn a_truncated_stream_ends_instead_of_blocking() {
        // 頭部だけで切れた場合。エディタが落ちた後に届く形である。
        let mut input = BufReader::new(&b"Content-Length: 10\r\n\r\n"[..]);
        assert!(read(&mut input).is_err(), "a truncated body should be reported");

        let mut empty = BufReader::new(&b""[..]);
        assert!(read(&mut empty).unwrap().is_none());
    }
}
