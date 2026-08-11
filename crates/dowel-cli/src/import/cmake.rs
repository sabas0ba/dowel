//! CMake File API（codemodel v2）からの取り込み。
//!
//! 写像の考え方と、下書きが未検証であることの扱いは
//! [`super`](super) に書いてある。ここは CMake の reply を読む側である。
//!
//! ## CMake 固有の写像
//!
//! - `EXECUTABLE` → `bin`、`STATIC_LIBRARY` / `OBJECT_LIBRARY` → `lib`。
//!   `SHARED_LIBRARY` も静的な `lib` にする。共有ライブラリは作れるように
//!   なったが、書き出す記号の一覧が要る（ADR-0030）——reply はそれを
//!   答えない。当てずっぽうの一覧を置くより、その旨をコメントに残して
//!   読み手に委ねる。`UTILITY` は読み飛ばす
//! - 翻訳の引数は `compileGroups` で `defines` / `includes` /
//!   `compileCommandFragments` に分かれている。仕分けは reply の側が
//!   済ませており、こちらは写すだけでよい
//! - 同一プロジェクト内の依存は `dependencies` から `target(...)` に写す

use super::{is_compiled, push_unique, relativize, sanitize, write_draft, Imported};
use dowel_support::json::{parse, Json};
use std::path::{Path, PathBuf};

/// reply ディレクトリ。ビルドディレクトリと reply 自体の双方を受ける。
///
/// 見つからないことは誤りではない——渡された先が Meson のものかもしれない。
/// 判別は [`super::import`] が行う。
pub fn reply_dir(given: &Path) -> Option<PathBuf> {
    let nested = given.join(".cmake/api/v1/reply");
    for candidate in [nested, given.to_path_buf()] {
        let named = list(&candidate).unwrap_or_default();
        if candidate.is_dir() && named.iter().any(|n| n.starts_with("codemodel-v2-")) {
            return Some(candidate);
        }
    }
    None
}

/// 取り込みを始める道具の作り方。判別に失敗したときの案内に使う。
pub const HOW_TO_QUERY: &str = "\
  CMake: mkdir -p <build>/.cmake/api/v1/query && touch <build>/.cmake/api/v1/query/codemodel-v2\n\
         then re-run `cmake -B <build> ...`";

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
/// reply を読み、ソースディレクトリへ `dowel.toml` / `dowel.build` の
/// 下書きを書く。既存のマニフェストは上書きしない。
pub fn import(reply: &Path) -> Result<(), String> {
    let reply = reply.to_path_buf();
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

    write_draft("CMake", &source_dir, project, &targets)
}

/// target-*.json から1ターゲットを抽出する。取り込めない種別は `None`。
fn extract(t: &Json, source_dir: &Path) -> Option<Imported> {
    let name = t.get("name").and_then(Json::as_str)?;
    let ty = t.get("type").and_then(Json::as_str)?;
    let (kind, note) = match ty {
        "EXECUTABLE" => ("bin", None),
        "STATIC_LIBRARY" | "OBJECT_LIBRARY" => ("lib", None),
        "SHARED_LIBRARY" => {
            ("lib", Some("was a SHARED_LIBRARY. imported as a static library: dowel needs an explicit\n#       `exports` list for a shared one (ADR-0030), and the File API does not report it"))
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
