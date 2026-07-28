//! 文書の整合。**リンクが切れていないこと**と**索引に漏れが無いこと**。
//!
//! 文書は腐る。腐っても誰も落ちないため、腐ったまま残る。
//! ここは「腐ったら落ちる」機構であり、`diagnostics.rs` の網羅検査と同じ役目を負う。
//!
//! 検査するのは機械的に判定できるものだけである。内容が正しいかは見ない。
//! 見るのは以下の4つ。
//!
//! - 相対リンクの指す先が存在すること
//! - コードとスクリプトが名指しする文書が存在すること
//! - `docs/README.md` の一覧が `docs/` の中身と一致すること
//! - `docs/adr/README.md` の表が ADR の実体と一致すること
//!
//! 設計は [`docs/51-testing.md`](../../../docs/51-testing.md) にある。

mod common;

use common::repo_root;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 検査対象の Markdown。生成物とビルド成果物は見ない。
fn markdown_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = vec![root.join("README.md"), root.join("CLAUDE.md")];
    for dir in ["docs", "docs/adr", "tests/projects", "tests/projects/layered"] {
        let d = root.join(dir);
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out.retain(|p| p.exists());
    out
}

/// `[表示](対象)` の対象のうち、リポジトリ内を指す相対パスだけを拾う。
fn relative_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == ']' && i + 1 < bytes.len() && bytes[i + 1] == '(' {
            let start = i + 2;
            let mut j = start;
            while j < bytes.len() && bytes[j] != ')' {
                j += 1;
            }
            if j < bytes.len() {
                let target: String = bytes[start..j].iter().collect();
                // 外部と、同一文書内の見出しは対象外。
                let external = target.starts_with("http://")
                    || target.starts_with("https://")
                    || target.starts_with('#')
                    || target.starts_with("mailto:");
                if !external {
                    out.push(target);
                }
            }
            i = j;
        }
        i += 1;
    }
    out
}

#[test]
fn every_relative_link_in_the_documents_resolves() {
    let mut broken = Vec::new();
    for file in markdown_files() {
        let text = std::fs::read_to_string(&file).expect("cannot read the document");
        let dir = file.parent().expect("a file has a parent").to_path_buf();
        for target in relative_links(&text) {
            // 見出しへの参照は本体だけを見る。
            let path_part = target.split('#').next().unwrap_or(&target);
            if path_part.is_empty() {
                continue;
            }
            let resolved = dir.join(path_part);
            if !resolved.exists() {
                broken.push(format!(
                    "  {} -> {target}",
                    file.strip_prefix(repo_root()).unwrap_or(&file).display()
                ));
            }
        }
    }
    assert!(broken.is_empty(), "these links do not resolve:\n{}", broken.join("\n"));
}

/// コードやスクリプトの中で名指しされている文書。
///
/// 番号を付け替えたときに真っ先に切れるのがここである。本文中のリンクと違い
/// Markdown の書式を持たないため、別に拾う。
#[test]
fn every_document_named_from_the_source_exists() {
    let root = repo_root();
    let mut missing = BTreeSet::new();
    let mut found_any = false;
    for file in
        source_files(&root.join("crates")).into_iter().chain(source_files(&root.join("scripts")))
    {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for name in doc_paths(&text) {
            found_any = true;
            if !root.join(&name).exists() {
                missing.insert(format!(
                    "  {} names {name}",
                    file.strip_prefix(&root).unwrap_or(&file).display()
                ));
            }
        }
    }
    assert!(found_any, "the scan found no document references; it is probably broken");
    assert!(
        missing.is_empty(),
        "these documents are named from the source but do not exist:\n{}",
        missing.into_iter().collect::<Vec<_>>().join("\n")
    );
}

fn source_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            out.extend(source_files(&p));
        } else if p.extension().is_some_and(|x| x == "rs" || x == "sh" || x == "py") {
            out.push(p);
        }
    }
    out
}

/// `docs/<name>.md` の形をした言及を拾う。
fn doc_paths(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, _) in text.match_indices("docs/") {
        let rest = &text[i..];
        let end = rest
            .find(|c: char| {
                !(c.is_ascii_alphanumeric() || c == '/' || c == '-' || c == '_' || c == '.')
            })
            .unwrap_or(rest.len());
        let candidate = &rest[..end];
        if candidate.ends_with(".md") {
            out.insert(candidate.to_string());
        }
    }
    out
}

#[test]
fn the_document_map_lists_every_document() {
    // 文書を足して地図に書き忘れると、誰も辿り着けない文書ができる。
    let docs = repo_root().join("docs");
    let map = std::fs::read_to_string(docs.join("README.md")).expect("docs/README.md is missing");

    let mut unlisted = Vec::new();
    for e in std::fs::read_dir(&docs).expect("cannot read docs/").flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || name == "README.md" {
            continue;
        }
        if !map.contains(&format!("({name})")) {
            unlisted.push(name);
        }
    }
    unlisted.sort();
    assert!(
        unlisted.is_empty(),
        "these documents are not in docs/README.md:\n  {}",
        unlisted.join("\n  ")
    );
}

#[test]
fn the_adr_index_matches_the_records() {
    // ADR は「決定の一覧」としてしか読まれない。索引から漏れた決定は無いのと同じ。
    let adr = repo_root().join("docs/adr");
    let index =
        std::fs::read_to_string(adr.join("README.md")).expect("docs/adr/README.md is missing");

    let mut files: Vec<String> = std::fs::read_dir(&adr)
        .expect("cannot read docs/adr/")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".md") && n != "README.md")
        .collect();
    files.sort();

    let missing: Vec<&String> = files.iter().filter(|n| !index.contains(*n)).collect();
    assert!(
        missing.is_empty(),
        "these ADRs are not in the index:\n  {}",
        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  ")
    );

    // 逆向き。索引に載っているのに実体が無い行を拾う。
    for line in index.lines().filter(|l| l.contains("](0")) {
        for target in relative_links(line) {
            assert!(
                adr.join(&target).exists(),
                "the index points at `{target}`, which does not exist"
            );
        }
    }
    assert!(files.len() >= 7, "only {} ADRs were found; the scan is probably broken", files.len());
}
