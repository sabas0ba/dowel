//! マニフェスト言語の言語サーバ。
//!
//! [30-devexp.md](../../../docs/30-devexp.md) 3.2 の方針により、初期は診断と
//! ホバーに絞る。コアの別フロントエンドであり、説明の出所はスキーマそのもの
//! （`dowel schema dump` が出すものと同じ表）である。
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
//! パッケージの中の文書は、`check` と同じ範囲（[ADR-0010]、計画段まで）で
//! 診断する。ファイルを跨ぐ検査（`undeclared-dependency`、併合の衝突）は
//! ワークスペースの模型から、計画段の検査（glob 展開・パス解決・
//! ツールチェーン探索）は実際のファイル走査から出る。どちらも読むだけで、
//! 何も書かず、外部プロセスも起動しない。孤立した文書は1ファイルで決まる
//! 範囲に留める（issue #38）。
//! 何を出さないかは [`UNSUPPORTED`] に列挙し、検査で追跡する。
//!
//! [ADR-0010]: ../../../docs/adr/0010-check-scope.md

mod hover;
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
    ("unreadable-build", "the buffer overlay cannot reproduce an unreadable file on disk"),
    (
        "not-debuggable",
        "raised by `dowel debug` about the target it was asked for; the editor asks for none",
    ),
    (
        "missing-debug-stub",
        "triggered by `--target` under `dowel debug`; the editor has no `--target` and starts \
         no debug session",
    ),
    ("missing-runner", "triggered by `--target`, which is not part of any manifest"),
    (
        "unsupported-target",
        "triggered by the requested triple; the editor has no `--target` and would show a \
         permanent error the reader cannot clear",
    ),
    (
        "unfetchable-dependency",
        "the server never touches the network; fetching and its diagnostic belong to the CLI",
    ),
    (
        "unsatisfied-dependency",
        "resolving system packages runs pkg-config; the server starts no external processes",
    ),
    (
        "lockfile-drift",
        "reconciling the lock happens when the CLI resolves; the server starts no external processes",
    ),
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
            w.key("hoverProvider").bool(true);
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
            publish_all(docs)
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
            publish_all(docs)
        }

        (_, "textDocument/didSave") => {
            let Some(uri) = str_at(params, "textDocument.uri") else { return Vec::new() };
            let _ = uri;
            publish_all(docs)
        }

        (_, "textDocument/didClose") => {
            let Some(uri) = str_at(params, "textDocument.uri") else { return Vec::new() };
            docs.text.remove(&uri);
            // 閉じた時点で診断を消す。エディタは残った印を自分では落とさない。
            // 残りの文書は診断し直す。閉じた緩衝がディスクの内容を覆っていた
            // 場合、他の文書の診断が変わりうる。
            let mut out = vec![diagnostics_notification(&uri, &SourceMap::new(), &[])];
            out.extend(publish_all(docs));
            out
        }

        (rpc::Message::Request { id, .. }, "textDocument/hover") => {
            vec![rpc::response(id, |w| hover_result(docs, params, w))]
        }

        // 要求には必ず応える。応えないとエディタは待ち続ける。
        (rpc::Message::Request { id, .. }, other) => {
            vec![rpc::error(id, -32601, &format!("`{other}` is not implemented"))]
        }
        (rpc::Message::Notification { .. }, _) => Vec::new(),
    }
}

/// ホバーの応答。説明が無い位置では `null` を返す。
fn hover_result(docs: &Documents, params: &Json, w: &mut JsonWriter) {
    let Some((text, h)) = find_hover(docs, params) else {
        w.null();
        return;
    };
    // 範囲は本文の中のオフセットで決まる。位置の変換のためだけに写しを作る。
    let mut sm = SourceMap::new();
    let file = sm.add("hover", text);

    w.begin_object();
    w.key("contents").begin_object();
    w.field_str("kind", "markdown");
    w.field_str("value", &h.markdown);
    w.end_object();
    w.key("range");
    write_range(w, &sm, file, h.span);
    w.end_object();
}

fn find_hover(docs: &Documents, params: &Json) -> Option<(String, hover::Hover)> {
    let uri = str_at(params, "textDocument.uri")?;
    let text = docs.text.get(&uri)?;
    let line = params.path("position.line")?.as_i64()? as u32;
    let character = params.path("position.character")?.as_i64()? as u32;
    let offset = offset_of(text, line, character)?;
    let parsed = dowel_syntax::parse(text, FileId(0));
    let h = hover::at(&parsed.root, text, offset)?;
    Some((text.clone(), h))
}

/// LSP の位置をバイトオフセットにする。
///
/// 行は 0 始まり、桁は UTF-16 単位である。範囲の外を指す位置は `None`。
/// 位置は外から来るため、丸めずに断る。
fn offset_of(text: &str, line: u32, character: u32) -> Option<u32> {
    let mut start = 0usize;
    for _ in 0..line {
        start += text.get(start..)?.find('\n')? + 1;
    }
    let rest = text.get(start..)?;
    let line_text = rest.split('\n').next().unwrap_or(rest);
    let mut units = 0u32;
    for (i, c) in line_text.char_indices() {
        if units == character {
            return Some((start + i) as u32);
        }
        units += c.len_utf16() as u32;
    }
    // 行末はその行の長さと一致する。それを超える位置は断る。
    (units == character).then_some((start + line_text.len()) as u32)
}

fn str_at(params: &Json, path: &str) -> Option<String> {
    params.path(path).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// 開いている全ての文書の診断を作り直す。
///
/// 1つの編集は同じパッケージの他の文書の診断を変えうる
/// （`[features]` を直せば `dowel.build` 側の `unknown-feature` が消える）。
/// 開いている文書は少数で解析はミリ秒の桁であり、全部やり直すのが
/// 最も単純で、かつ取りこぼしが無い。
fn publish_all(docs: &Documents) -> Vec<String> {
    docs.text.keys().map(|uri| publish(docs, uri)).collect()
}

/// この文書が属するパッケージのルート。
///
/// マニフェスト（`dowel.toml`）が同じディレクトリに開かれているか、
/// ディスクに在る場合にだけパッケージとみなす。どちらも無ければ
/// 1ファイルで決まる検査に留める（孤立した `dowel.build` を編集中でも
/// 説明が出るように）。
fn package_root(path: &std::path::Path, docs: &Documents) -> Option<std::path::PathBuf> {
    let name = path.file_name()?;
    if name != dowel_model::session::MANIFEST_NAME && name != dowel_model::session::BUILD_NAME {
        return None;
    }
    let dir = path.parent()?;
    let manifest = dir.join(dowel_model::session::MANIFEST_NAME);
    if docs.text.keys().any(|u| path_of(u) == manifest) || manifest.exists() {
        Some(dir.to_path_buf())
    } else {
        None
    }
}

/// パッケージの模型で診断する。ファイルを跨ぐ検査はここで出る。
///
/// 開いている緩衝が正本であり、模型はそれを重ねて読む
/// （`Session::load_for_editor`）。ネットワークにもストアにも触れない。
/// 打鍵ごとに作って捨てるため、常駐デーモンとは区別されたままである
/// （[ADR-0002]）。
///
/// 解析の根は、開いている各マニフェストのディレクトリと自分のディレクトリの
/// 全てを試す。依存の先を編集しているとき（併合の衝突の片割れ等）、その文書に
/// 掛かる診断は依存元を根とする模型からしか出ないためである。自分に届かない
/// 根からは何も採られず、同じ診断が複数の根から届いた場合は1つに畳む。
///
/// [ADR-0002]: ../../../docs/adr/0002-no-daemon.md
/// エディタ用の構成。CLI の `configure` と同じ判断で組む。
///
/// 機能フラグは読み込みの段で解決した集合をそのまま使い、ツールチェーンは
/// 根のパッケージの宣言を反映する。反映しないと、宣言されたコンパイラでは
/// なく既定の `cc` を探してしまい、`missing-toolchain` が manifest の記述と
/// 食い違う。対象はホストのトリプルに固定する（`--target` に相当する入力は
/// エディタに無い）。
fn editor_config(sess: &dowel_model::Session) -> dowel_eval::Config {
    let mut cfg = dowel_eval::Config::host_default();
    if let Some(root) = sess.root_package() {
        sess.configure(&mut cfg);
        let host = dowel_eval::config::default_triple();
        if let Some(decl) = root.toolchain_for(&cfg.target, &host) {
            for (name, _) in dowel_eval::config::TOOLS {
                if let Some(t) = decl.tool(name) {
                    cfg.set_tool(name, t.command.clone());
                }
            }
        }
    }
    cfg
}

fn publish_workspace(docs: &Documents, uri: &str, path: &std::path::Path) -> String {
    let overlay: BTreeMap<std::path::PathBuf, String> =
        docs.text.iter().map(|(u, t)| (path_of(u), t.clone())).collect();

    let mut roots: std::collections::BTreeSet<std::path::PathBuf> = overlay
        .keys()
        .filter(|p| p.file_name().is_some_and(|n| n == dowel_model::session::MANIFEST_NAME))
        .filter_map(|p| p.parent().map(std::path::Path::to_path_buf))
        .collect();
    if let Some(dir) = path.parent() {
        roots.insert(dir.to_path_buf());
    }

    let mut batches: Vec<(dowel_model::Session, Vec<Diagnostic>)> = Vec::new();
    let mut seen: std::collections::BTreeSet<(String, String, u32, u32)> =
        std::collections::BTreeSet::new();
    for root in &roots {
        let sess = dowel_model::Session::load_for_editor(root, overlay.clone());
        let cfg = editor_config(&sess);
        let (graph, gdiags) = dowel_model::graph::build(&sess, &cfg);
        let idiags = dowel_model::interface::prepare(&sess, &graph, &cfg);
        let mut diags = sess.diagnostics.clone();
        diags.extend(gdiags);
        diags.extend(idiags);
        // 計画段まで通す（`check` と同じ範囲、ADR-0010）。glob 展開・
        // パス解決・ツールチェーンの実在検査はファイルシステムを読むだけで、
        // 何も書かず、外部プロセスも起動しない。併合の診断（衝突・ABI
        // 不一致）も `compile_env` を経由してこの中で出る。
        let all: Vec<dowel_model::TargetId> = sess.targets.iter().map(|t| t.id).collect();
        let (_, pdiags) = dowel_build::plan::plan(&sess, &graph, &cfg, &all);
        diags.extend(pdiags);
        // この文書に主ラベルを持つものだけを、この文書へ出す。他の開いている
        // 文書に掛かる診断は、その文書の publish が同じ模型から拾い直す。
        let kept: Vec<Diagnostic> = diags
            .into_iter()
            .filter(|d| {
                let Some(l) = d.primary_label() else { return false };
                if !(sess.sm.contains(l.file) && sess.sm.path(l.file) == path) {
                    return false;
                }
                seen.insert((d.code.to_string(), d.message.clone(), l.span.start, l.span.end))
            })
            .collect();
        if !kept.is_empty() {
            batches.push((sess, kept));
        }
    }

    log_debug!(
        "lsp: {} workspace diagnostics for {uri}",
        batches.iter().map(|(_, d)| d.len()).sum::<usize>()
    );
    rpc::notification("textDocument/publishDiagnostics", |w| {
        w.begin_object();
        w.field_str("uri", uri);
        w.key("diagnostics").begin_array();
        for (sess, diags) in &batches {
            for d in diags {
                write_diagnostic(w, &sess.sm, d);
            }
        }
        w.end_array();
        w.end_object();
    })
}

/// 1ファイルを解析・評価して診断の通知を作る。
fn publish(docs: &Documents, uri: &str) -> String {
    let text = docs.text.get(uri).map(|s| s.as_str()).unwrap_or("");
    let path = path_of(uri);
    // パッケージの中の文書は、ワークスペースの模型で診断する。
    if package_root(&path, docs).is_some() {
        return publish_workspace(docs, uri, &path);
    }
    let mut sm = SourceMap::new();
    let file = sm.add(&path, text.to_string());

    let parsed = dowel_syntax::parse(text, file);
    let mut diags = parsed.diagnostics.clone();
    // `dowel.toml` は厳密な TOML として保つ（ADR-0003）。名前で判別する。
    if path.file_name().is_some_and(|n| n == dowel_model::session::MANIFEST_NAME) {
        diags.extend(dowel_eval::strict::check(&parsed.root, file));
    }
    let (doc, eval_diags) = dowel_eval::eval(&parsed.root, text, file);
    diags.extend(eval_diags);
    // 型検査の段。開いている1ファイルで決まる検査は CLI と同じ実装で出す
    // （issue #38）。出さないと、誤りのあるマニフェストがエディタでは
    // 無傷に見える。ファイルを跨ぐ検査は [`UNSUPPORTED`] に列挙してある。
    match path.file_name() {
        Some(n) if n == dowel_model::session::MANIFEST_NAME => {
            diags.extend(dowel_model::session::check_manifest_file(&doc, file));
        }
        Some(n) if n == dowel_model::session::BUILD_NAME => {
            diags.extend(dowel_model::session::check_build_file(&doc));
        }
        _ => {}
    }

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
