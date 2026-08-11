//! Meson の introspect 出力からの取り込み。
//!
//! 写像の考え方と、下書きが未検証であることの扱いは
//! [`super`](super) に書いてある。ここは `meson-info/` を読む側である。
//!
//! ## Meson 固有の写像
//!
//! - `executable` → `bin`、`static library` → `lib`。`shared library` も
//!   静的な `lib` にする。共有ライブラリは作れるが、書き出す記号の一覧が
//!   要り（ADR-0030）、introspect はそれを答えない。当てずっぽうを置かず、
//!   その旨をコメントに残す。`custom` / `run` は読み飛ばす
//! - **翻訳の引数は仕分けられていない。** CMake の reply が `defines` /
//!   `includes` / フラグに分けて答えるのに対し、Meson は
//!   `target_sources[].parameters` に1つの配列で渡してくる。仕分けは
//!   [`super::classify_argument`] が行う
//! - **ターゲット間の依存は写せない。** introspect の出力に、あるターゲットが
//!   どのターゲットにリンクするかが無い。推測すると——出力ファイル名の
//!   突き合わせなどで——当たらないものを `deps` に書くことになる。書かずに、
//!   下書きの読み手に委ねる。`migrate verify` は翻訳の引数を突き合わせる
//!   ものであり、この欠けはリンクの段で現れる
//! - サブプロジェクト（`subproject` が空でないターゲット）は読み飛ばす。
//!   別のパッケージであり、1つの `dowel.build` に混ぜると出所が消える

use super::{classify_argument, is_compiled, push_unique, relativize, write_draft, Imported};
use dowel_support::json::{parse, Json};
use std::path::{Path, PathBuf};

/// 取り込みを始める道具の作り方。判別に失敗したときの案内に使う。
pub const HOW_TO_QUERY: &str = "\
  Meson: `meson setup <build> <source>` writes <build>/meson-info/ on its own";

/// `meson-info` ディレクトリ。ビルドディレクトリと `meson-info` 自体の双方を受ける。
///
/// 見つからないことは誤りではない——渡された先が CMake のものかもしれない。
/// 判別は [`super::import`] が行う。
pub fn info_dir(given: &Path) -> Option<PathBuf> {
    [given.join("meson-info"), given.to_path_buf()]
        .into_iter()
        .find(|candidate| candidate.join("intro-targets.json").is_file())
}

fn read_json(dir: &Path, name: &str) -> Result<Json, String> {
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse(&text).ok_or_else(|| format!("{} is not valid JSON", path.display()))
}

pub fn import(info: &Path) -> Result<(), String> {
    let targets_json = read_json(info, "intro-targets.json")?;
    let entries = targets_json.as_array().ok_or("intro-targets.json is not an array of targets")?;

    // 名前と、ソースの木の場所。`intro-projectinfo.json` は meson が
    // 一緒に書くが、無くても取り込みは続けられる——名前が既定に落ちるだけ。
    let info_json = read_json(info, "intro-projectinfo.json").ok();
    let project = info_json
        .as_ref()
        .and_then(|j| j.get("descriptive_name"))
        .and_then(Json::as_str)
        .unwrap_or("imported");

    // ソースディレクトリは introspect が直接は答えない。`defined_in`
    // （`meson.build` の絶対パス）を持つ最上位のターゲットから採る——
    // サブプロジェクトを除いた中で最も浅いものが、木の根の `meson.build`
    // である。
    let source_dir = source_dir(entries)
        .ok_or("cannot tell where the source tree is: no target reports a `defined_in`")?;

    let mut targets = Vec::new();
    for t in entries {
        if let Some(imported) = extract(t, &source_dir) {
            targets.push(imported);
        }
    }
    write_draft("Meson", &source_dir, project, &targets)
}

/// ソースの木の根。
///
/// サブプロジェクトのターゲットは数えない。別の木に住んでおり、混ぜると
/// 根が上へずれる。
fn source_dir(entries: &[Json]) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for t in entries {
        if is_subproject(t) {
            continue;
        }
        let Some(defined_in) = t.get("defined_in").and_then(Json::as_str) else { continue };
        let Some(dir) = Path::new(defined_in).parent() else { continue };
        // 最も浅いものを採る。深いものはサブディレクトリの `meson.build`。
        let deeper =
            best.as_ref().is_some_and(|b| b.components().count() <= dir.components().count());
        if !deeper {
            best = Some(dir.to_path_buf());
        }
    }
    best
}

fn is_subproject(t: &Json) -> bool {
    t.get("subproject").and_then(Json::as_str).is_some_and(|s| !s.is_empty())
}

/// intro-targets.json の1件を抽出する。取り込めない種別は `None`。
fn extract(t: &Json, source_dir: &Path) -> Option<Imported> {
    if is_subproject(t) {
        return None;
    }
    let name = t.get("name").and_then(Json::as_str)?;
    let ty = t.get("type").and_then(Json::as_str)?;
    let (kind, note) = match ty {
        "executable" => ("bin", None),
        "static library" => ("lib", None),
        "shared library" | "shared module" => {
            ("lib", Some("was a shared library. imported as a static library: dowel needs an explicit\n#       `exports` list for a shared one (ADR-0030), and introspection does not report it"))
        }
        _ => return None,
    };

    let mut out = Imported::new(kind, name, note);
    for group in t.get("target_sources").and_then(Json::as_array).unwrap_or(&[]) {
        for s in group.get("sources").and_then(Json::as_array).unwrap_or(&[]) {
            let Some(path) = s.as_str() else { continue };
            if !is_compiled(path) {
                continue;
            }
            match relativize(path, source_dir) {
                Some(rel) => push_unique(&mut out.sources, rel),
                None => push_unique(&mut out.skipped_sources, path.to_string()),
            }
        }
        // 生成されたソースは写せない。生成する規則は dowel 側に無く、
        // 黙って落とすと下書きが「組めるように見えて足りない」形になる。
        for s in group.get("generated_sources").and_then(Json::as_array).unwrap_or(&[]) {
            if let Some(path) = s.as_str() {
                push_unique(&mut out.skipped_sources, format!("{path} (generated)"));
            }
        }
        for p in group.get("parameters").and_then(Json::as_array).unwrap_or(&[]) {
            if let Some(arg) = p.as_str() {
                classify_argument(arg, source_dir, &mut out);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(text: &str) -> Vec<Json> {
        parse(text).unwrap().as_array().unwrap().to_vec()
    }

    #[test]
    fn the_source_tree_is_the_shallowest_meson_build() {
        let entries = targets(
            r#"[{"name": "deep", "defined_in": "/proj/lib/meson.build"},
                {"name": "root", "defined_in": "/proj/meson.build"}]"#,
        );
        assert_eq!(source_dir(&entries), Some(PathBuf::from("/proj")));
    }

    #[test]
    fn subprojects_do_not_move_the_root() {
        // サブプロジェクトは別の木に住む。数えると根が上へずれる。
        let entries = targets(
            r#"[{"name": "vendored", "subproject": "zlib",
                 "defined_in": "/proj/subprojects/zlib/meson.build"},
                {"name": "root", "defined_in": "/proj/meson.build"}]"#,
        );
        assert_eq!(source_dir(&entries), Some(PathBuf::from("/proj")));
    }

    #[test]
    fn a_targets_arguments_are_sorted_out_of_one_list() {
        let entries = targets(
            r#"[{"name": "len", "type": "static library", "defined_in": "/proj/meson.build",
                 "target_sources": [{"language": "c",
                    "parameters": ["-I/proj/lib", "-DLIMIT=64", "-Wall", "-O2"],
                    "sources": ["/proj/lib/len.c", "/proj/lib/len.h"]}]}]"#,
        );
        let t = extract(&entries[0], Path::new("/proj")).expect("the target is importable");
        assert_eq!(t.kind, "lib");
        // ヘッダは翻訳の対象ではない。
        assert_eq!(t.sources, ["lib/len.c"]);
        assert_eq!(t.includes, ["lib"]);
        assert_eq!(t.defines, ["LIMIT=64"]);
        // `-O2` は構成が供給する。写さない。
        assert_eq!(t.flags, ["-Wall"]);
    }

    #[test]
    fn a_generated_source_is_named_rather_than_dropped() {
        // 生成の規則は写せない。黙って落とすと、下書きが「組めるように
        // 見えて足りない」形になる。
        let entries = targets(
            r#"[{"name": "app", "type": "executable", "defined_in": "/proj/meson.build",
                 "target_sources": [{"sources": ["/proj/src/main.c"],
                                     "generated_sources": ["/build/version.c"]}]}]"#,
        );
        let t = extract(&entries[0], Path::new("/proj")).expect("the target is importable");
        assert_eq!(t.sources, ["src/main.c"]);
        assert!(t
            .skipped_sources
            .iter()
            .any(|s| s.contains("version.c") && s.contains("generated")));
    }

    #[test]
    fn the_kinds_dowel_cannot_build_are_skipped_or_noted() {
        let entries = targets(
            r#"[{"name": "gen", "type": "custom", "defined_in": "/p/meson.build"},
                {"name": "so", "type": "shared library", "defined_in": "/p/meson.build"}]"#,
        );
        assert!(extract(&entries[0], Path::new("/p")).is_none());
        let so = extract(&entries[1], Path::new("/p")).expect("a shared library still imports");
        assert_eq!(so.kind, "lib");
        assert!(so.note.is_some_and(|n| n.contains("shared")));
    }
}
