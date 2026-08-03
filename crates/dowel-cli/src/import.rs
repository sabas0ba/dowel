//! 移行の下書き生成（`dowel migrate import`、docs/40-migration.md 4節）。
//!
//! CMake File API（codemodel v2）の reply を読み、マニフェストの下書きを
//! **ソースディレクトリへ**生成する。出力は成果物ではなく下書きである:
//! 抽出できるのは特定の OS・構成・依存解決結果での一射影であり、条件分岐と
//! `public` / `private` の意図は失われている（docs/40-migration.md 3節）。
//!
//! したがって（Q6 の有力案のとおり）生成物には未検証の印を付ける。
//! 印は先頭のコメントであり、`dowel migrate verify` で旧システムの
//! `compile_commands.json` と突き合わせる導線をそこに書く。
//!
//! ## 写像
//!
//! - `EXECUTABLE` → `bin`、`STATIC_LIBRARY` / `OBJECT_LIBRARY` → `lib`。
//!   `SHARED_LIBRARY` も `lib` にする（共有ライブラリは未実装のため、
//!   その旨をコメントに残す）。`UTILITY` は読み飛ばす
//! - ソースは glob にしない。射影を忠実に写す下書きでは、拾う集合を
//!   広げない方が `verify` と食い違わない
//! - `public` / `private` は判別できないため全て `private` に置く。
//!   公開ヘッダの昇格は人間の仕事として残る
//! - 外部ライブラリ（`-l...`）は `link_flags` へ。同一プロジェクト内の
//!   依存は `dependencies` から `target(...)` に写す
//! - 構成レベルのフラグ（build type 由来の `-O` / `-g` / `-DNDEBUG`）は
//!   写さない。dowel では `cfg.opt` が供給するものであり、無条件の
//!   `flags` に写すと構成の切り替えと衝突する（issue #54）

use dowel_support::json::{parse, Json};
use std::path::{Path, PathBuf};

/// reply ディレクトリを見つける。ビルドディレクトリと reply 自体の双方を受ける。
fn reply_dir(given: &Path) -> Result<PathBuf, String> {
    let nested = given.join(".cmake/api/v1/reply");
    for candidate in [&nested, &given.to_path_buf()] {
        if candidate.is_dir() && list(candidate)?.iter().any(|n| n.starts_with("codemodel-v2-")) {
            return Ok(candidate.clone());
        }
    }
    Err(format!(
        "`{}` has no CMake File API reply. run cmake with a `codemodel-v2` query first:\n  \
         mkdir -p <build>/.cmake/api/v1/query && touch <build>/.cmake/api/v1/query/codemodel-v2 && \
         cmake -B <build> ...",
        given.display()
    ))
}

fn list(dir: &Path) -> Result<Vec<String>, String> {
    Ok(std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect())
}

fn read_json(dir: &Path, name: &str) -> Result<Json, String> {
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse(&text).ok_or_else(|| format!("{} is not valid JSON", path.display()))
}

/// 1ターゲット分の抽出結果。
struct Imported {
    kind: &'static str,
    name: String,
    sources: Vec<String>,
    skipped_sources: Vec<String>,
    includes: Vec<String>,
    external_includes: Vec<String>,
    defines: Vec<String>,
    flags: Vec<String>,
    link_flags: Vec<String>,
    deps: Vec<String>,
    note: Option<&'static str>,
}

/// reply を読み、ソースディレクトリへ `dowel.toml` / `dowel.build` の
/// 下書きを書く。既存のマニフェストは上書きしない。
pub fn import(given: &Path) -> Result<(), String> {
    let reply = reply_dir(given)?;
    let codemodel_name = list(&reply)?
        .into_iter()
        .find(|n| n.starts_with("codemodel-v2-"))
        .expect("reply_dir checked this");
    let codemodel = read_json(&reply, &codemodel_name)?;

    let source_dir = codemodel
        .path("paths.source")
        .and_then(Json::as_str)
        .ok_or("the codemodel has no source path")?;
    let source_dir = PathBuf::from(source_dir);
    let config = codemodel
        .path("configurations")
        .and_then(Json::as_array)
        .and_then(|c| c.first())
        .ok_or("the codemodel has no configuration")?;
    let project = config
        .path("projects")
        .and_then(Json::as_array)
        .and_then(|p| p.first())
        .and_then(|p| p.get("name"))
        .and_then(Json::as_str)
        .unwrap_or("imported");

    // ターゲット id → 名前。`dependencies` の写像に使う。
    let target_entries = config.path("targets").and_then(Json::as_array).unwrap_or(&[]);
    let mut targets = Vec::new();
    for entry in target_entries {
        let Some(json_file) = entry.get("jsonFile").and_then(Json::as_str) else { continue };
        let t = read_json(&reply, json_file)?;
        if let Some(imported) = extract(&t, &source_dir) {
            targets.push(imported);
        }
    }
    if targets.is_empty() {
        return Err("the codemodel contains no importable targets".into());
    }

    let manifest_path = source_dir.join("dowel.toml");
    let build_path = source_dir.join("dowel.build");
    for p in [&manifest_path, &build_path] {
        if p.exists() {
            return Err(format!("`{}` already exists; not overwriting it", p.display()));
        }
    }

    std::fs::write(&manifest_path, render_manifest(project))
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;
    std::fs::write(&build_path, render_build(&targets))
        .map_err(|e| format!("cannot write {}: {e}", build_path.display()))?;

    eprintln!("imported {} target(s) into {}", targets.len(), source_dir.display());
    eprintln!("the draft is UNVERIFIED. check it against the old build:");
    eprintln!(
        "  dowel -C {} migrate verify <old-build>/compile_commands.json",
        source_dir.display()
    );
    Ok(())
}

/// target-*.json から1ターゲットを抽出する。取り込めない種別は `None`。
fn extract(t: &Json, source_dir: &Path) -> Option<Imported> {
    let name = t.get("name").and_then(Json::as_str)?;
    let ty = t.get("type").and_then(Json::as_str)?;
    let (kind, note) = match ty {
        "EXECUTABLE" => ("bin", None),
        "STATIC_LIBRARY" | "OBJECT_LIBRARY" => ("lib", None),
        "SHARED_LIBRARY" => {
            ("lib", Some("was a SHARED_LIBRARY; dowel builds static archives today"))
        }
        _ => return None,
    };

    let mut out = Imported {
        kind,
        name: sanitize(name),
        sources: Vec::new(),
        skipped_sources: Vec::new(),
        includes: Vec::new(),
        external_includes: Vec::new(),
        defines: Vec::new(),
        flags: Vec::new(),
        link_flags: Vec::new(),
        deps: Vec::new(),
        note,
    };

    for s in t.get("sources").and_then(Json::as_array).unwrap_or(&[]) {
        let Some(path) = s.get("path").and_then(Json::as_str) else { continue };
        // ヘッダはコンパイル対象ではない。File API はソース一覧に含める。
        if !is_compiled(path) {
            continue;
        }
        match relativize(path, source_dir) {
            Some(rel) => out.sources.push(rel),
            None => out.skipped_sources.push(path.to_string()),
        }
    }

    for g in t.get("compileGroups").and_then(Json::as_array).unwrap_or(&[]) {
        for d in g.get("defines").and_then(Json::as_array).unwrap_or(&[]) {
            if let Some(def) = d.get("define").and_then(Json::as_str) {
                push_unique(&mut out.defines, def.to_string());
            }
        }
        for i in g.get("includes").and_then(Json::as_array).unwrap_or(&[]) {
            if let Some(path) = i.get("path").and_then(Json::as_str) {
                match relativize(path, source_dir) {
                    Some(rel) => push_unique(&mut out.includes, rel),
                    None => push_unique(&mut out.external_includes, path.to_string()),
                }
            }
        }
        for f in g.get("compileCommandFragments").and_then(Json::as_array).unwrap_or(&[]) {
            if let Some(frag) = f.get("fragment").and_then(Json::as_str) {
                for word in frag.split_whitespace() {
                    // 構成レベルのフラグ（`CMAKE_<LANG>_FLAGS_<CONFIG>` 由来の
                    // `-O` / `-g` / `-DNDEBUG`）は写さない。dowel では `cfg.opt`
                    // が供給するもので、写すと無条件のフラグになり、release
                    // から取り込んだ下書きの debug ビルドが最適化された
                    // `NDEBUG` 付きになる（issue #54）。
                    if dowel_build::migrate::is_config_flag(word) {
                        continue;
                    }
                    push_unique(&mut out.flags, word.to_string());
                }
            }
        }
    }

    for f in t.path("link.commandFragments").and_then(Json::as_array).unwrap_or(&[]) {
        let role = f.get("role").and_then(Json::as_str).unwrap_or("");
        let Some(frag) = f.get("fragment").and_then(Json::as_str) else { continue };
        // 同一プロジェクト内の成果物は `deps` が張る。外部ライブラリの
        // 指定だけを写す。
        if role == "libraries" || role == "flags" {
            for word in frag.split_whitespace().filter(|w| w.starts_with("-l") || role == "flags") {
                // 構成レベルのフラグは翻訳側（compileCommandFragments）と
                // 同じ判定でリンク側からも落とす。落とさないと、見出しの
                // 「写していない」と中身が食い違い、-flto 構成では debug の
                // リンク時最適化が -O3 で回る（issue #61）。
                if dowel_build::migrate::is_config_flag(word) {
                    continue;
                }
                push_unique(&mut out.link_flags, word.to_string());
            }
        }
    }

    for d in t.get("dependencies").and_then(Json::as_array).unwrap_or(&[]) {
        if let Some(id) = d.get("id").and_then(Json::as_str) {
            // id は `名前::@ハッシュ`。
            if let Some(dep_name) = id.split("::").next() {
                push_unique(&mut out.deps, sanitize(dep_name));
            }
        }
    }

    Some(out)
}

fn is_compiled(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("");
    matches!(ext, "c" | "cc" | "cp" | "cpp" | "cxx" | "c++" | "CPP" | "C" | "s" | "S")
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// ソースディレクトリ配下なら相対にする。外は `None`。
fn relativize(path: &str, source_dir: &Path) -> Option<String> {
    let p = Path::new(path);
    let joined;
    let abs = if p.is_absolute() {
        p
    } else {
        joined = source_dir.join(p);
        &joined
    };
    abs.strip_prefix(source_dir).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
}

/// dowel の識別子に写す。CMake のターゲット名はより広い文字を許す。
fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    if !out.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        out.insert(0, 't');
    }
    out
}

const HEADER: &str = "\
# GENERATED by `dowel migrate import` - UNVERIFIED DRAFT.
#
# This is a snapshot of one CMake configuration. Conditionals are lost, and
# the public/private intent of includes and defines is unknowable from the
# File API, so everything is declared private. Promote what dependents need
# to a `public` block, then verify against the old build:
#
#   dowel migrate verify <old-build>/compile_commands.json
#
# Configuration-level flags (-O / -g / -DNDEBUG from the CMake build type)
# were NOT copied: dowel's own debug/release configuration supplies them.
#
";

fn render_manifest(project: &str) -> String {
    format!("{HEADER}\n[package]\nname    = \"{}\"\nversion = \"0.0.0\"\n", sanitize(project))
}

fn render_build(targets: &[Imported]) -> String {
    let mut out = String::from(HEADER);
    for t in targets {
        out.push('\n');
        if let Some(note) = t.note {
            out.push_str(&format!("# NOTE: {note}\n"));
        }
        out.push_str(&format!("[{}.{}]\n", t.kind, t.name));
        out.push_str("sources = [\n");
        for s in &t.sources {
            out.push_str(&format!("    file(\"{s}\"),\n"));
        }
        out.push_str("]\n");
        for s in &t.skipped_sources {
            out.push_str(&format!("# skipped source (outside the source tree): {s}\n"));
        }

        let mut private = String::new();
        if !t.includes.is_empty() {
            let items: Vec<String> = t.includes.iter().map(|i| format!("dir(\"{i}\")")).collect();
            private.push_str(&format!("includes = [{}]\n", items.join(", ")));
        }
        if !t.defines.is_empty() {
            let items: Vec<String> = t.defines.iter().map(|d| render_define(d)).collect();
            private.push_str(&format!("defines  = {{ {} }}\n", items.join(", ")));
        }
        let mut flags = t.flags.clone();
        // ソースの木の外のインクルードは `dir()` にできない。フラグとして写す。
        for i in &t.external_includes {
            flags.push(format!("-I{i}"));
        }
        if !flags.is_empty() {
            let items: Vec<String> = flags.iter().map(|f| format!("\"{f}\"")).collect();
            private.push_str(&format!("flags    = [{}]\n", items.join(", ")));
        }
        if !t.link_flags.is_empty() {
            let items: Vec<String> = t.link_flags.iter().map(|f| format!("\"{f}\"")).collect();
            private.push_str(&format!("link_flags = [{}]\n", items.join(", ")));
        }
        if !t.deps.is_empty() {
            let items: Vec<String> = t.deps.iter().map(|d| format!("target(\"{d}\")")).collect();
            private.push_str(&format!("deps     = [{}]\n", items.join(", ")));
        }
        if !private.is_empty() {
            out.push_str(&format!("\n[{}.{}.private]\n{private}", t.kind, t.name));
        }
    }
    out
}

/// `NAME=値` を `defines` の1項目にする。数値はそのまま、他は文字列。
fn render_define(def: &str) -> String {
    match def.split_once('=') {
        None => format!("{def} = 1"),
        Some((k, v)) if v.parse::<i64>().is_ok() => format!("{k} = {v}"),
        Some((k, v)) => format!("{k} = {:?}", v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defines_keep_numbers_and_quote_the_rest() {
        assert_eq!(render_define("FOO"), "FOO = 1");
        assert_eq!(render_define("LIMIT=64"), "LIMIT = 64");
        assert_eq!(render_define("NAME=abc"), "NAME = \"abc\"");
    }

    #[test]
    fn cmake_names_are_mapped_to_identifiers() {
        assert_eq!(sanitize("my.target"), "my-target");
        assert_eq!(sanitize("2fast"), "t2fast");
        assert_eq!(sanitize("ok_name"), "ok_name");
    }

    #[test]
    fn paths_inside_the_source_tree_become_relative() {
        let src = Path::new("/proj");
        assert_eq!(relativize("/proj/src/a.c", src), Some("src/a.c".into()));
        assert_eq!(relativize("src/a.c", src), Some("src/a.c".into()));
        assert_eq!(relativize("/usr/include", src), None);
    }
}
