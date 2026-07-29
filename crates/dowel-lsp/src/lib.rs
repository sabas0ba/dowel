//! マニフェスト言語の言語サーバ。
//!
//! [30-devexp.md](../../../docs/30-devexp.md) 3.2 の方針により、初期は診断に
//! 絞る。コアの別フロントエンドであり、解析と評価は CLI と同じクエリを通す。
//!
//! ## 常駐との関係
//!
//! [ADR-0002](../../../docs/adr/0002-no-daemon.md) は常駐デーモンを持たないと
//! 定めるが、言語サーバはその例外である。エディタが起動主体でありエディタと
//! 共に終了するため、デーモンとは区別される。CLI は言語サーバの存在に
//! 一切依存しない。
//!
//! ## 現時点で見ているもの
//!
//! 開いているファイル1つを単位として、構文解析と評価の診断を出す。
//! ファイルを跨ぐ診断（`undeclared-dependency`、併合の衝突）は
//! ワークスペースの模型を要するため、まだ出さない。
//! 何を出さないかは [`unsupported`] に列挙し、検査で追跡する。

mod rpc;

use dowel_support::json::{Json, JsonWriter};
use dowel_support::{log_debug, Diagnostic, FileId, Severity, SourceMap};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};

/// まだ出さない診断と、その理由。
///
/// 空にするのが目標である。「言語サーバでは出ない」ことを知らずに使うと、
/// 誤りのあるマニフェストが正しいものに見える。
pub const UNSUPPORTED: &[(&str, &str)] = &[
    ("undeclared-dependency", "resolving `dep(...)` needs the workspace model"),
    ("unknown-target", "resolving `target(...)` needs the other targets of the package"),
    ("unknown-feature", "the vocabulary comes from `[features]` of the same package"),
    ("merge-conflict", "merging needs the dependency graph"),
    ("abi-mismatch", "merging needs the dependency graph"),
    ("invalid-source", "path resolution happens at plan time"),
    ("unresolved-path", "path resolution happens at plan time"),
    ("empty-glob", "glob expansion happens at plan time"),
];

/// 開いている文書。エディタの緩衝が正本であり、ディスクは見ない。
#[derive(Default)]
struct Documents {
    /// URI → 本文
    text: BTreeMap<String, String>,
}

/// 標準入出力の上でサーバを回す。`exit` を受け取るか流れが尽きると終わる。
pub fn serve(input: &mut impl BufRead, output: &mut impl Write) -> std::io::Result<()> {
    let mut docs = Documents::default();
    let mut shutdown_requested = false;

    while let Some(message) = rpc::read(input)? {
        log_debug!("lsp: {}", message.method());
        let replies = handle(&mut docs, &message, &mut shutdown_requested);
        for body in replies {
            rpc::write(output, &body)?;
        }
        if message.method() == "exit" {
            break;
        }
    }
    Ok(())
}

/// 1件を処理し、書き出す本文を返す。
///
/// 入出力から切り離してあるのは、検査が本文の対応だけを見られるようにするため。
fn handle(docs: &mut Documents, m: &rpc::Message, shutdown: &mut bool) -> Vec<String> {
    let params = m.params();
    match (m, m.method()) {
        (rpc::Message::Request { id, .. }, "initialize") => vec![rpc::response(id, |w| {
            w.begin_object();
            w.key("capabilities").begin_object();
            // 全文同期。差分同期は本文の再構成が要り、得るものは大きくない。
            w.key("textDocumentSync").i64(1);
            w.end_object();
            w.key("serverInfo").begin_object();
            w.field_str("name", "dowel");
            w.field_str("version", env!("CARGO_PKG_VERSION"));
            w.end_object();
            w.end_object();
        })],

        (rpc::Message::Request { id, .. }, "shutdown") => {
            *shutdown = true;
            vec![rpc::response(id, |w| {
                w.null();
            })]
        }

        (_, "initialized" | "exit") => Vec::new(),

        (_, "textDocument/didOpen") => {
            let Some(uri) = str_at(params, "textDocument.uri") else { return Vec::new() };
            let text = str_at(params, "textDocument.text").unwrap_or_default();
            docs.text.insert(uri.clone(), text);
            vec![publish(docs, &uri)]
        }

        (_, "textDocument/didChange") => {
            let Some(uri) = str_at(params, "textDocument.uri") else { return Vec::new() };
            // 全文同期のため、変更は常に1件で本文全体を持つ。
            let Some(text) = params
                .path("contentChanges")
                .and_then(|c| c.as_array())
                .and_then(|c| c.last())
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
            else {
                return Vec::new();
            };
            docs.text.insert(uri.clone(), text.to_string());
            vec![publish(docs, &uri)]
        }

        (_, "textDocument/didSave") => {
            let Some(uri) = str_at(params, "textDocument.uri") else { return Vec::new() };
            vec![publish(docs, &uri)]
        }

        (_, "textDocument/didClose") => {
            let Some(uri) = str_at(params, "textDocument.uri") else { return Vec::new() };
            docs.text.remove(&uri);
            // 閉じた時点で診断を消す。エディタは残った印を自分では落とさない。
            vec![diagnostics_notification(&uri, &SourceMap::new(), &[])]
        }

        // 要求には必ず応える。応えないとエディタは待ち続ける。
        (rpc::Message::Request { id, .. }, other) => {
            vec![rpc::error(id, -32601, &format!("`{other}` is not implemented"))]
        }
        (rpc::Message::Notification { .. }, _) => Vec::new(),
    }
}

fn str_at(params: &Json, path: &str) -> Option<String> {
    params.path(path).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// 1ファイルを解析・評価して診断の通知を作る。
fn publish(docs: &Documents, uri: &str) -> String {
    let text = docs.text.get(uri).map(|s| s.as_str()).unwrap_or("");
    let path = path_of(uri);
    let mut sm = SourceMap::new();
    let file = sm.add(&path, text.to_string());

    let parsed = dowel_syntax::parse(text, file);
    let mut diags = parsed.diagnostics.clone();
    // `dowel.toml` は厳密な TOML として保つ（ADR-0003）。名前で判別する。
    if path.file_name().is_some_and(|n| n == dowel_model::session::MANIFEST_NAME) {
        diags.extend(dowel_eval::strict::check(&parsed.root, file));
    }
    let (_, eval_diags) = dowel_eval::eval(&parsed.root, text, file);
    diags.extend(eval_diags);

    log_debug!("lsp: {} diagnostics for {uri}", diags.len());
    diagnostics_notification(uri, &sm, &diags)
}

/// `file:` の URI をパスにする。
///
/// 他の書式は扱わない。パスが取れない場合も名前だけは残す。
/// 診断の位置は本文の中のオフセットで決まり、パスには依存しない。
fn path_of(uri: &str) -> std::path::PathBuf {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    std::path::PathBuf::from(percent_decode(rest))
}

/// URI の百分率符号化を解く。解けない並びはそのまま残す。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn diagnostics_notification(uri: &str, sm: &SourceMap, diags: &[Diagnostic]) -> String {
    rpc::notification("textDocument/publishDiagnostics", |w| {
        w.begin_object();
        w.field_str("uri", uri);
        w.key("diagnostics").begin_array();
        for d in diags {
            write_diagnostic(w, sm, d);
        }
        w.end_array();
        w.end_object();
    })
}

fn write_diagnostic(w: &mut JsonWriter, sm: &SourceMap, d: &Diagnostic) {
    w.begin_object();
    w.key("range");
    match d.primary_label() {
        Some(l) => write_range(w, sm, l.file, l.span),
        // 位置を持たない診断はファイルの先頭に置く。捨てると誤りが見えなくなる。
        None => write_range(w, sm, FileId(0), dowel_support::Span::EMPTY),
    }
    w.key("severity").i64(match d.severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Note => 3,
    });
    w.field_str("code", d.code);
    w.field_str("source", "dowel");
    // 注記と主ラベルの説明を本文に畳む。エディタは1つの文字列しか出さない。
    let mut message = d.message.clone();
    if let Some(l) = d.primary_label().filter(|l| !l.message.is_empty()) {
        message.push_str(&format!("\n{}", l.message));
    }
    for note in &d.notes {
        message.push_str(&format!("\nnote: {note}"));
    }
    for s in &d.suggestions {
        message.push_str(&format!("\nhelp: {}", s.message));
    }
    w.field_str("message", &message);
    w.end_object();
}

/// LSP の位置は 0 始まりの行と、UTF-16 単位の桁である。
fn write_range(w: &mut JsonWriter, sm: &SourceMap, file: FileId, span: dowel_support::Span) {
    w.begin_object();
    w.key("start");
    write_position(w, sm, file, span.start);
    w.key("end");
    write_position(w, sm, file, span.end);
    w.end_object();
}

fn write_position(w: &mut JsonWriter, sm: &SourceMap, file: FileId, offset: u32) {
    let lc = sm.line_col(file, offset);
    let line = lc.line.saturating_sub(1);
    // `line_col` の桁は文字数である。LSP は UTF-16 単位を既定とするため、
    // 行頭からの本文を測り直す。
    let text = sm.line_text(file, lc.line);
    let chars = lc.col.saturating_sub(1) as usize;
    let utf16: usize = text.chars().take(chars).map(|c| c.len_utf16()).sum();
    w.begin_object();
    w.key("line").u64(line as u64);
    w.key("character").u64(utf16 as u64);
    w.end_object();
}

#[cfg(test)]
mod tests;
