//! 言語サーバの検査。
//!
//! 標準入出力の上で本文をやり取りする形をそのまま試す。要求を組み立てて
//! [`serve`] に流し、返ってきた本文を読み解く。エディタが見るものと同じ
//! 経路であり、枠付けと本文の対応もまとめて確かめられる。

use super::*;
use dowel_support::json::parse;

/// 枠付けした本文を並べて入力にする。
fn stream(bodies: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for b in bodies {
        write!(&mut out, "Content-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    }
    out
}

fn request(id: i64, method: &str, params: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
}

fn notification(method: &str, params: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#)
}

fn did_open(uri: &str, text: &str) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.key("textDocument").begin_object();
    w.field_str("uri", uri);
    w.field_str("languageId", "dowel");
    w.key("version").i64(1);
    w.field_str("text", text);
    w.end_object();
    w.end_object();
    notification("textDocument/didOpen", &w.finish())
}

fn did_change(uri: &str, text: &str) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.key("textDocument").begin_object();
    w.field_str("uri", uri);
    w.key("version").i64(2);
    w.end_object();
    w.key("contentChanges").begin_array();
    w.begin_object();
    w.field_str("text", text);
    w.end_object();
    w.end_array();
    w.end_object();
    notification("textDocument/didChange", &w.finish())
}

/// 一連の本文を流し、返ってきた本文を順に返す。
fn exchange(bodies: &[String]) -> Vec<Json> {
    let bytes = stream(bodies);
    let mut input = std::io::BufReader::new(&bytes[..]);
    let mut output: Vec<u8> = Vec::new();
    serve(&mut input, &mut output).expect("the server failed");

    // 返ってきた流れを枠付けから解く。
    let mut out = Vec::new();
    let mut rest = &output[..];
    while !rest.is_empty() {
        let text = String::from_utf8_lossy(rest).into_owned();
        let head = text.find("\r\n\r\n").expect("a reply without a header");
        let len: usize = text[..head]
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length:"))
            .expect("no Content-Length")
            .trim()
            .parse()
            .expect("an unreadable Content-Length");
        let body_start = head + 4;
        let body = &rest[body_start..body_start + len];
        out.push(parse(&String::from_utf8_lossy(body)).expect("a reply that is not json"));
        rest = &rest[body_start + len..];
    }
    out
}

/// 通知の中の診断コード。
fn codes(reply: &Json) -> Vec<String> {
    reply
        .path("params.diagnostics")
        .and_then(|d| d.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|d| d.get("code"))
                .filter_map(|c| c.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn initialize_announces_full_document_sync() {
    let out = exchange(&[request(1, "initialize", "{}")]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].path("id").and_then(|i| i.as_i64()), Some(1));
    assert_eq!(
        out[0].path("result.capabilities.textDocumentSync").and_then(|s| s.as_i64()),
        Some(1)
    );
    assert_eq!(out[0].path("result.serverInfo.name").and_then(|n| n.as_str()), Some("dowel"));
}

#[test]
fn opening_a_file_publishes_its_diagnostics() {
    let out =
        exchange(&[did_open("file:///w/dowel.build", "[bin.app]\nsourcess = glob(\"src/*.c\")\n")]);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].path("method").and_then(|m| m.as_str()),
        Some("textDocument/publishDiagnostics")
    );
    assert_eq!(out[0].path("params.uri").and_then(|u| u.as_str()), Some("file:///w/dowel.build"));
    // 型検査の段も1ファイルで決まる範囲は出す（issue #38）。出さないと、
    // 最も踏みやすい誤りがエディタでは無傷に見える。
    assert_eq!(codes(&out[0]), ["unknown-property"]);
}

#[test]
fn a_syntax_error_reaches_the_editor_with_a_range() {
    let out =
        exchange(&[did_open("file:///w/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\n")]);
    let d = &out[0].path("params.diagnostics").unwrap().as_array().unwrap()[0];
    assert_eq!(d.get("code").and_then(|c| c.as_str()), Some("unterminated-string"));
    assert_eq!(d.get("source").and_then(|s| s.as_str()), Some("dowel"));
    // 重大度は誤り。
    assert_eq!(d.get("severity").and_then(|s| s.as_i64()), Some(1));
    // 位置は 0 始まり。2行目に出る。
    assert_eq!(d.path("range.start.line").and_then(|l| l.as_i64()), Some(1));
}

#[test]
fn editing_replaces_the_previous_diagnostics() {
    // 誤りを直したら消えること。消えないと、直したはずの印が残り続ける。
    let uri = "file:///w/dowel.build";
    let out = exchange(&[
        did_open(uri, "[bin.app]\nsources = glob(\"src/*.c\n"),
        did_change(uri, "[bin.app]\nsources = glob(\"src/*.c\")\n"),
    ]);
    assert_eq!(out.len(), 2);
    assert!(codes(&out[0]).contains(&"unterminated-string".to_string()), "{:?}", codes(&out[0]));
    assert!(codes(&out[1]).is_empty(), "the diagnostics were not cleared: {:?}", codes(&out[1]));
}

#[test]
fn closing_a_file_clears_its_diagnostics() {
    let uri = "file:///w/dowel.build";
    let out = exchange(&[
        did_open(uri, "[bin.app]\nsources = glob(\"src/*.c\n"),
        notification("textDocument/didClose", &format!(r#"{{"textDocument":{{"uri":"{uri}"}}}}"#)),
    ]);
    assert_eq!(out.len(), 2);
    assert!(codes(&out[1]).is_empty());
    assert_eq!(out[1].path("params.uri").and_then(|u| u.as_str()), Some(uri));
}

#[test]
fn the_manifest_is_held_to_strict_toml() {
    // `dowel.toml` と `dowel.build` の区別はファイル名で行う（ADR-0003）。
    // 同じ「値の位置の式」が、`dowel.toml` では拒まれ `dowel.build` では通る。
    let expr = "flags = match cfg.opt { debug => [\"-O0\"], release => [\"-O2\"] }\n";
    let manifest =
        exchange(&[did_open("file:///w/dowel.toml", &format!("[package]\nname = \"a\"\n{expr}"))]);
    // マニフェストだけが開いているため、パッケージの模型は `dowel.build` の
    // 不在も併せて指摘する。
    assert_eq!(codes(&manifest[0]), ["expression-in-strict-toml", "missing-build"]);

    let build = exchange(&[did_open(
        "file:///w/dowel.build",
        &format!("[bin.a]\nsources = glob(\"src/*.c\")\n\n[bin.a.private]\n{expr}"),
    )]);
    assert!(codes(&build[0]).is_empty(), "{:?}", codes(&build[0]));
}

fn hover_request(id: i64, uri: &str, line: i64, character: i64) -> String {
    request(
        id,
        "textDocument/hover",
        &format!(
            r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":{line},"character":{character}}}}}"#
        ),
    )
}

#[test]
fn hover_is_announced_and_answered() {
    let uri = "file:///w/dowel.build";
    let out = exchange(&[
        request(1, "initialize", "{}"),
        did_open(uri, "[lib.foo.public]\nincludes = [dir(\"include\")]\n"),
        // 2行目の先頭は `includes`。
        hover_request(2, uri, 1, 2),
    ]);
    assert_eq!(
        out[0].path("result.capabilities.hoverProvider").and_then(|h| h.as_bool()),
        Some(true)
    );

    let value = out[2].path("result.contents.value").and_then(|v| v.as_str()).unwrap_or("");
    assert!(value.contains("`includes`"), "{value}");
    assert!(value.contains("merge: `union`"), "{value}");
    assert_eq!(out[2].path("result.contents.kind").and_then(|k| k.as_str()), Some("markdown"));
    // 範囲は語を覆う。`includes` は8文字。
    assert_eq!(out[2].path("result.range.start.character").and_then(|c| c.as_i64()), Some(0));
    assert_eq!(out[2].path("result.range.end.character").and_then(|c| c.as_i64()), Some(8));
}

#[test]
fn hover_on_a_position_without_a_word_answers_null() {
    // 応答しないとエディタは待ち続ける。説明が無いことは `null` で伝える。
    let uri = "file:///w/dowel.build";
    let out = exchange(&[did_open(uri, "[lib.foo]\n"), hover_request(1, uri, 1, 0)]);
    assert_eq!(out[1].path("result"), Some(&Json::Null));
}

#[test]
fn hover_on_a_file_that_is_not_open_answers_null() {
    let out = exchange(&[hover_request(1, "file:///w/never-opened.build", 0, 0)]);
    assert_eq!(out[0].path("result"), Some(&Json::Null));
}

#[test]
fn the_hover_position_is_read_in_utf16_units() {
    // 桁は UTF-16 単位で来る。文字数として扱うと非 ASCII の行でずれる。
    let uri = "file:///w/dowel.build";
    let text = "[bin.a.private]\nflags = [\"あ😀\"] when feature.fast\n";
    // `feature.fast` は `flags = ["あ😀"] when ` の後ろ。
    let prefix = "flags = [\"あ😀\"] when ";
    let character: i64 = prefix.chars().map(|c| c.len_utf16() as i64).sum();
    let out = exchange(&[did_open(uri, text), hover_request(1, uri, 1, character + 1)]);
    let value = out[1].path("result.contents.value").and_then(|v| v.as_str()).unwrap_or("");
    assert!(value.contains("`feature.fast`"), "{value}");
}

#[test]
fn a_position_outside_the_text_answers_null_instead_of_being_rounded() {
    // 位置は外から来る。丸めると別の語を説明することになる。
    let uri = "file:///w/dowel.build";
    let out = exchange(&[
        did_open(uri, "[lib.foo]\n"),
        hover_request(1, uri, 99, 0),
        hover_request(2, uri, 0, 99),
    ]);
    assert_eq!(out[1].path("result"), Some(&Json::Null));
    assert_eq!(out[2].path("result"), Some(&Json::Null));
}

#[test]
fn the_end_of_a_line_is_a_valid_position() {
    // 行末はその行の長さと一致する。断ってはならない。
    assert_eq!(offset_of("ab\ncd\n", 0, 2), Some(2));
    assert_eq!(offset_of("ab\ncd\n", 1, 2), Some(5));
    assert_eq!(offset_of("ab\ncd\n", 1, 3), None);
    assert_eq!(offset_of("ab\ncd\n", 5, 0), None);
    // 非 ASCII を含む行。`😀` は UTF-16 で2単位を占める。
    assert_eq!(offset_of("a😀b\n", 0, 3), Some("a😀".len() as u32));
}

#[test]
fn a_request_that_is_not_implemented_gets_an_error_instead_of_silence() {
    // 応えないとエディタは待ち続ける。
    let out = exchange(&[request(9, "textDocument/completion", "{}")]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].path("id").and_then(|i| i.as_i64()), Some(9));
    assert_eq!(out[0].path("error.code").and_then(|c| c.as_i64()), Some(-32601));
}

#[test]
fn shutdown_is_answered_and_exit_ends_the_loop() {
    let out = exchange(&[
        request(1, "shutdown", "null"),
        notification("exit", "null"),
        // `exit` の後は読まない。ここに来た本文への応答は現れない。
        request(2, "initialize", "{}"),
    ]);
    assert_eq!(out.len(), 1, "the server kept reading after exit");
    assert_eq!(out[0].path("id").and_then(|i| i.as_i64()), Some(1));
}

#[test]
fn an_unknown_notification_is_ignored() {
    // 通知には応答しない。知らない method で落ちないこと。
    let out = exchange(&[notification("$/setTrace", r#"{"value":"verbose"}"#)]);
    assert!(out.is_empty());
}

#[test]
fn the_column_is_measured_in_utf16_units() {
    // LSP の桁は UTF-16 単位である。文字数で出すと非 ASCII を含む行でずれる。
    // `あ` は UTF-16 で1単位、`😀` は2単位。誤りは同じ行の後方に置く。
    let text = "[bin.app]\nsources = glob(\"あ😀\") @\n";
    let out = exchange(&[did_open("file:///w/dowel.build", text)]);
    let d = &out[0].path("params.diagnostics").unwrap().as_array().unwrap()[0];
    assert_eq!(d.get("code").and_then(|c| c.as_str()), Some("unknown-char"));
    assert_eq!(d.path("range.start.line").and_then(|l| l.as_i64()), Some(1));
    // `@` の前は 21 文字。`😀` が2単位を占めるため UTF-16 では 22。
    let prefix = "sources = glob(\"あ😀\") ";
    assert_eq!(prefix.chars().count(), 21);
    assert_eq!(d.path("range.start.character").and_then(|c| c.as_i64()), Some(22));
}

#[test]
fn a_percent_encoded_uri_becomes_a_path() {
    // 名前で `dowel.toml` を判別するため、URI の復号が要る。
    assert_eq!(path_of("file:///w/my%20project/dowel.toml").file_name().unwrap(), "dowel.toml");
    assert_eq!(percent_decode("a%2Fb"), "a/b");
    // 解けない並びはそのまま残す。
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
}

#[test]
fn the_unsupported_list_names_codes_that_exist() {
    // 直したのに一覧が残ると、出ない診断があると誤解させる。
    // 綴りの誤りも同じく検出する。
    let declared = declared_codes();
    for (code, _) in UNSUPPORTED {
        assert!(declared.contains(*code), "`{code}` is listed but no code emits it");
    }
}

#[test]
fn nothing_in_the_unsupported_list_is_published() {
    // 一覧に載せた診断が実際には出ている場合、一覧の方が古い。
    let uri = "file:///w/dowel.build";
    let out = exchange(&[did_open(
        uri,
        "[bin.app]\nsources = glob(\"nowhere/*.c\")\n\n\
         [bin.app.private]\ndeps = [dep(\"absent\"), target(\"absent\")]\n",
    )]);
    let published = codes(&out[0]);
    for (code, _) in UNSUPPORTED {
        assert!(
            !published.contains(&code.to_string()),
            "`{code}` is published after all; remove it from UNSUPPORTED"
        );
    }
}

/// ソースに現れる安定コード。`diagnostics.rs` の走査と同じ形。
fn declared_codes() -> std::collections::BTreeSet<String> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join("crates");
    let mut out = std::collections::BTreeSet::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "tests" || n == "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                let text = match text.find("#[cfg(test)]") {
                    Some(i) => text[..i].to_string(),
                    None => text,
                };
                for (i, _) in text
                    .match_indices("Diagnostic::error(")
                    .chain(text.match_indices("Diagnostic::warning("))
                {
                    let rest = &text[i..];
                    let Some(open) = rest.find('(') else { continue };
                    let after = rest[open + 1..].trim_start();
                    let Some(body) = after.strip_prefix('"') else { continue };
                    let Some(end) = body.find('"') else { continue };
                    let code = &body[..end];
                    if !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    {
                        out.insert(code.to_string());
                    }
                }
            }
        }
    }
    out
}

#[test]
fn cross_file_diagnostics_come_from_the_package_model() {
    // `[features]` の語彙は `dowel.toml` が決める。両方が開いていれば、
    // `dowel.build` 側の未知の機能名はファイルを跨いで検査される（issue #38）。
    let toml = "[package]\nname = \"a\"\n\n[features]\ndefault = []\nreal    = []\n";
    let build = "[bin.a]\nsources = glob(\"src/*.c\")\n\n[bin.a.private]\nflags = [\"-DX\"] when feature.raal\n";
    let out = exchange(&[
        did_open("file:///w/dowel.toml", toml),
        did_open("file:///w/dowel.build", build),
    ]);
    let last_for = |name: &str| {
        out.iter()
            .rev()
            .find(|m| {
                m.path("params.uri").and_then(|u| u.as_str()).is_some_and(|u| u.ends_with(name))
            })
            .expect("the document got a notification")
    };
    assert_eq!(codes(last_for("dowel.build")), ["unknown-feature"]);
    assert!(codes(last_for("dowel.toml")).is_empty());

    // 語彙の側を直せば、`dowel.build` の診断も消える。
    let fixed = exchange(&[
        did_open("file:///w/dowel.toml", toml),
        did_open("file:///w/dowel.build", build),
        did_change(
            "file:///w/dowel.toml",
            "[package]\nname = \"a\"\n\n[features]\ndefault = []\nraal    = []\n",
        ),
    ]);
    let last = fixed
        .iter()
        .rev()
        .find(|m| {
            m.path("params.uri")
                .and_then(|u| u.as_str())
                .is_some_and(|u| u.ends_with("dowel.build"))
        })
        .expect("the build file got a notification");
    assert!(codes(last).is_empty(), "{:?}", codes(last));
}

#[test]
fn a_conflict_in_a_dependency_is_reported_at_the_arriving_value() {
    // 併合の衝突の主ラベルは依存側のファイルにある。依存元のマニフェストが
    // 開いていれば、その模型が依存先の文書にも診断を届ける。
    let out = exchange(&[
        did_open(
            "file:///w/app/dowel.toml",
            "[package]\nname = \"app\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
        ),
        did_open(
            "file:///w/app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps    = [dep(\"lib\")]\ndefines = { LIMIT = 128 }\n",
        ),
        did_open("file:///w/lib/dowel.toml", "[package]\nname = \"lib\"\nversion = \"0\"\n"),
        did_open(
            "file:///w/lib/dowel.build",
            "[lib.lib]\nsources = glob(\"src/*.c\")\n\n[lib.lib.public]\ndefines = { LIMIT = 64 }\n",
        ),
    ]);
    let last = out
        .iter()
        .rev()
        .find(|m| {
            m.path("params.uri")
                .and_then(|u| u.as_str())
                .is_some_and(|u| u.ends_with("lib/dowel.build"))
        })
        .expect("the dependency's build file got a notification");
    assert_eq!(codes(last), ["merge-conflict"]);
}
