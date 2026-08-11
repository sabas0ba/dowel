//! 文書の整合性検査。リンクの解決可能性と、索引の完全性を確かめる。
//!
//! 文書の不整合はビルドにもテストにも影響しないため、検査しない限り検出されない。
//! `diagnostics.rs` の網羅検査と同じく、放置を防ぐための機械的な検査である。
//!
//! 対象は機械的に判定できる項目に限る。記述内容の妥当性は検査しない。
//! 検査項目は以下の6つ。
//!
//! - 相対リンクの指す先が存在すること
//! - コードとスクリプトが名指しする文書が存在すること
//! - `docs/README.md` の一覧が `docs/` の中身と一致すること
//! - `docs/adr/README.md` の表が ADR の実体と一致すること
//! - `_data/nav.yml`（サイトの目次）が `docs/` の中身と一致すること
//! - スキーマが受け付ける鍵が `12-build-reference.md` に書かれていること
//!
//! 設計は [`docs/51-testing.md`](../../../docs/51-testing.md) にある。

mod common;

use common::repo_root;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 検査対象の Markdown。生成物とビルド成果物は対象外とする。
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
                // 外部 URL と同一文書内の見出しは対象外とする。
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
            // 見出し付きの参照はパス部分のみを検査する。
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

/// コードやスクリプトの中で参照されている文書。
///
/// 文書番号を変更した場合、Markdown のリンクより先にこちらが解決しなくなる。
/// Markdown の書式を持たないため、別途走査する。
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
    // 一覧に記載しなかった文書は、参照経路を持たないまま残る。
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

/// サイトの目次（`_data/nav.yml`）と `docs/` の対応。
///
/// 目次は GitHub Pages の頁の枠にしか現れないため、外れても手元では気付けない。
/// 文書を足して載せ忘れれば、その頁はサイト上で辿れないまま残る。
#[test]
fn the_site_navigation_matches_the_documents() {
    let root = repo_root();
    let nav =
        std::fs::read_to_string(root.join("_data/nav.yml")).expect("_data/nav.yml is missing");

    // YAML の解釈はしない。`path:` の行だけを拾えば足りる。
    let listed: Vec<String> = nav
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("path:"))
        .map(|p| p.trim().to_string())
        .collect();
    assert!(
        listed.len() >= 15,
        "only {} entries were found; the scan is probably broken",
        listed.len()
    );

    // サイト上の住所から、元の Markdown を導く。
    // `/docs/adr/` のような索引は、そのディレクトリの README を指す。
    let source_of = |path: &str| -> String {
        let rel = path.trim_start_matches('/');
        if let Some(dir) = rel.strip_suffix('/') {
            format!("{dir}/README.md")
        } else {
            rel.replace(".html", ".md")
        }
    };

    let mut missing = Vec::new();
    for path in &listed {
        let source = source_of(path);
        if !root.join(&source).exists() {
            missing.push(format!("  {path} -> {source}"));
        }
    }
    assert!(
        missing.is_empty(),
        "these entries of _data/nav.yml point at documents that do not exist:\n{}",
        missing.join("\n")
    );

    // 逆方向。目次に無い文書は、サイト上で辿る道を持たない。
    let sources: BTreeSet<String> = listed.iter().map(|p| source_of(p)).collect();
    let mut unlisted = Vec::new();
    for e in std::fs::read_dir(root.join("docs")).expect("cannot read docs/").flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        if !sources.contains(&format!("docs/{name}")) {
            unlisted.push(name);
        }
    }
    unlisted.sort();
    assert!(
        unlisted.is_empty(),
        "these documents are not in _data/nav.yml:\n  {}",
        unlisted.join("\n  ")
    );
}

#[test]
fn the_crate_table_matches_the_workspace() {
    // クレートを足して表に書かないと、読み手はその層が無いものとして読む。
    // 実装状況の文書は「何が在るか」の索引でもある。
    let status = repo_root().join("docs/91-implementation-status.md");
    let text =
        std::fs::read_to_string(&status).expect("docs/91-implementation-status.md is missing");

    let mut found: Vec<String> = std::fs::read_dir(repo_root().join("crates"))
        .expect("cannot read crates/")
        .flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    found.sort();
    assert!(
        found.len() >= 8,
        "only {} crates were found; the scan is probably broken",
        found.len()
    );

    // 本文のどこかに名前が出ているだけでは足りない。表の行として在ることを見る。
    let listed: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("| `dowel-"))
        .map(|l| l.trim_start_matches("| `").split('`').next().unwrap_or(""))
        .collect();

    let missing: Vec<&String> = found.iter().filter(|c| !listed.contains(&c.as_str())).collect();
    assert!(
        missing.is_empty(),
        "these crates are not in the table of docs/91-implementation-status.md:\n  {}",
        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  ")
    );

    // 逆方向。表にあるが実体の無いクレートを検出する。
    for name in listed {
        assert!(
            found.iter().any(|c| c == name),
            "the table names `{name}`, which is not a crate in the workspace"
        );
    }
}

#[test]
fn the_adr_index_matches_the_records() {
    // ADR は索引経由で参照される。索引に無い決定は事実上参照できない。
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

    // 逆方向。索引に記載があるが実体が存在しない行を検出する。
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

/// スキーマが受け付ける鍵が、全て `12-build-reference.md` に書かれていること。
///
/// 同頁は冒頭で「この頁とエディタと診断が黙って食い違うことはない」と
/// 述べている。その約束を支えていたものは何も無く、実際に `cases` が
/// 型検査器にだけ存在する状態になっていた（issue #90）。
///
/// 検査は節ごとに行う。頁のどこかに同じ綴りがあれば通る形にすると、
/// `args` のように複数の表に現れる名前で素通しになる。
#[test]
fn every_property_the_schema_accepts_is_in_the_reference() {
    use dowel_eval::schema;
    let text = std::fs::read_to_string(repo_root().join("docs/12-build-reference.md"))
        .expect("the build reference is part of the repository");

    // 節の見出しに含まれる綴りで引く。表を1つ足した者は、ここで
    // 「どこに書くのか」を決めることになる。
    let heading = |t: &schema::NestedTable| match t.word {
        schema::ARTIFACTS => "`[<kind>.<name>.artifacts]`",
        schema::INSPECT => "`[<kind>.<name>.inspect]`",
        schema::CASES => "`[test.<name>.cases]`",
        schema::HARNESS => "`[test.<name>.harness]`",
        other => panic!("`{other}` has no section in docs/12-build-reference.md"),
    };
    let mut sections: Vec<(String, Vec<schema::PropDef>)> = vec![
        ("`[<kind>.<name>]`".to_string(), schema::root_props()),
        ("`[<kind>.<name>.public]`".to_string(), schema::block_props()),
        ("`[runner.<triple>]`".to_string(), schema::runner_props()),
    ];
    for t in schema::NESTED_TABLES {
        sections.push((heading(t).to_string(), (t.props)()));
    }

    for (title, props) in sections {
        let start = text
            .find(&format!("### {title}"))
            .unwrap_or_else(|| panic!("no section `### {title} …` in docs/12-build-reference.md"));
        let rest = &text[start + 4..];
        // 次の見出しまで。節の水準は問わない——最後の節は `## 4.` で終わる。
        let end = ["\n### ", "\n## "]
            .iter()
            .filter_map(|h| rest.find(h))
            .min()
            .map(|i| start + 4 + i)
            .unwrap_or(text.len());
        let section = &text[start..end];
        for p in props {
            assert!(
                section.contains(&format!("| `{}` |", p.name)),
                "`{}` is accepted under {title} but has no row in docs/12-build-reference.md",
                p.name
            );
        }
    }
}
