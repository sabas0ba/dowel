//! 移行の下書き生成（`dowel migrate import`、docs/40-migration.md 4節）。
//!
//! 既存のビルド系が**自分で答えたもの**を読み、マニフェストの下書きを
//! ソースディレクトリへ生成する。出力は成果物ではなく下書きである:
//! 抽出できるのは特定の OS・構成・依存解決結果での一射影であり、条件分岐と
//! `public` / `private` の意図は失われている（docs/40-migration.md 3節）。
//!
//! したがって（Q6 の有力案のとおり）生成物には未検証の印を付ける。
//! 印は先頭のコメントであり、`dowel migrate verify` で旧システムの
//! `compile_commands.json` と突き合わせる導線をそこに書く。
//!
//! ## 読む相手
//!
//! | 元 | 読むもの |
//! |---|---|
//! | CMake | File API の reply（codemodel v2） |
//! | Meson | `meson-info/` の introspect 出力 |
//!
//! どちらであるかは**渡されたディレクトリを見て**決める。利用者に
//! `--from=cmake` のような札を書かせない——渡すのは旧ビルドディレクトリで
//! あり、それが何で作られたかはそこに書いてある。
//!
//! ## どちらにも共通の写像
//!
//! - ソースは glob にしない。射影を忠実に写す下書きでは、拾う集合を
//!   広げない方が `verify` と食い違わない
//! - `public` / `private` は判別できないため全て `private` に置く。
//!   公開ヘッダの昇格は人間の仕事として残る
//! - 構成レベルのフラグ（build type 由来の `-O` / `-g` / `-DNDEBUG`）は
//!   写さない。dowel では `cfg.opt` が供給するものであり、無条件の
//!   `flags` に写すと構成の切り替えと衝突する（issue #54）

mod cmake;
mod meson;

use std::path::Path;

/// 1ターゲット分の抽出結果。
pub struct Imported {
    pub kind: &'static str,
    pub name: String,
    pub sources: Vec<String>,
    pub skipped_sources: Vec<String>,
    pub includes: Vec<String>,
    pub external_includes: Vec<String>,
    pub defines: Vec<String>,
    pub flags: Vec<String>,
    pub link_flags: Vec<String>,
    pub deps: Vec<String>,
    /// 翻訳の引数として渡されたが、旗ではなかったもの（issue #135）。
    /// 書庫の名前や `ar` の引数がここに落ちる
    pub dropped_inputs: Vec<String>,
    pub note: Option<&'static str>,
}

impl Imported {
    pub fn new(kind: &'static str, name: &str, note: Option<&'static str>) -> Imported {
        Imported {
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
            dropped_inputs: Vec::new(),
            note,
        }
    }
}

/// 渡されたディレクトリを読み、下書きを書く。
///
/// 元のビルド系は見て決める。どちらの記録も無ければ、両方の作り方を述べる
/// ——「何を渡せばよいか」が分からないまま断られるのが一番困る。
pub fn import(given: &Path) -> Result<(), String> {
    if let Some(reply) = cmake::reply_dir(given) {
        return cmake::import(&reply);
    }
    if let Some(info) = meson::info_dir(given) {
        return meson::import(&info);
    }
    Err(format!(
        "`{}` holds neither a CMake File API reply nor Meson introspection data.\n\
         point `migrate import` at the build directory of the old system, after asking it to \
         describe itself:\n{}\n{}",
        given.display(),
        cmake::HOW_TO_QUERY,
        meson::HOW_TO_QUERY,
    ))
}

/// 翻訳の引数を1つずつ仕分ける。
///
/// CMake の reply は `defines` / `includes` / フラグに分けて答えるが、
/// Meson は1つの配列で渡してくる。仕分けの規則を1箇所に置き、どちらから
/// 来ても同じ判断になるようにする。
pub fn classify_argument(arg: &str, source_dir: &Path, out: &mut Imported) {
    // 構成レベルのフラグは写さない（issue #54）。
    if dowel_build::migrate::is_config_flag(arg) {
        return;
    }
    if let Some(path) = arg.strip_prefix("-I") {
        // `-I` の引数が空のことがある。`dir("")` は下書きを読みにくくする
        // だけで、何も指していない。
        if path.is_empty() {
            return;
        }
        match relativize(path, source_dir) {
            Some(rel) => push_unique(&mut out.includes, rel),
            None => push_unique(&mut out.external_includes, path.to_string()),
        }
        return;
    }
    if let Some(def) = arg.strip_prefix("-D") {
        push_unique(&mut out.defines, def.to_string());
        return;
    }
    // リンカへの引数は翻訳の引数ではない（issue #135）。`flags` に入れると
    // コンパイラに渡り、`-Wl,--start-group` のような対で書くものが片方だけ
    // 翻訳のたびに現れる。
    if arg.starts_with("-Wl,") || arg.starts_with("-l") || arg.starts_with("-L") {
        push_unique(&mut out.link_flags, arg.to_string());
        return;
    }
    // 旗でないものは落とす。Meson の `parameters` には、書庫の名前
    // （`libshapes.a`）と `ar` の引数（`csrDT`）が混ざる。`cc` はこれらを
    // 入力ファイルとして読もうとし、下書きはそのままでは組めない。
    //
    // `deps` に写せないのは既存の判断のままである（introspect はどの
    // ターゲットに繋ぐかを答えない）。それなら、リンクの入力を翻訳の
    // 引数として残す理由も無い。
    if !arg.starts_with('-') {
        push_unique(&mut out.dropped_inputs, arg.to_string());
        return;
    }
    push_unique(&mut out.flags, arg.to_string());
}

pub fn is_compiled(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("");
    matches!(ext, "c" | "cc" | "cp" | "cpp" | "cxx" | "c++" | "CPP" | "C" | "s" | "S")
}

pub fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// ソースディレクトリ配下なら相対にする。外は `None`。
pub fn relativize(path: &str, source_dir: &Path) -> Option<String> {
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

/// dowel の識別子に写す。既存のビルド系のターゲット名はより広い文字を許す。
pub fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    if !out.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        out.insert(0, 't');
    }
    out
}

/// 下書きを書き出す。既存のマニフェストは上書きしない。
pub fn write_draft(
    system: &str,
    source_dir: &Path,
    project: &str,
    targets: &[Imported],
) -> Result<(), String> {
    if targets.is_empty() {
        return Err(format!("the {system} description contains no importable targets"));
    }
    let manifest_path = source_dir.join("dowel.toml");
    let build_path = source_dir.join("dowel.build");
    for p in [&manifest_path, &build_path] {
        if p.exists() {
            return Err(format!("`{}` already exists; not overwriting it", p.display()));
        }
    }

    let header = header(system);
    std::fs::write(&manifest_path, render_manifest(&header, project))
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;
    std::fs::write(&build_path, render_build(&header, targets))
        .map_err(|e| format!("cannot write {}: {e}", build_path.display()))?;

    eprintln!("imported {} target(s) into {}", targets.len(), source_dir.display());
    eprintln!("the draft is UNVERIFIED. check it against the old build:");
    eprintln!(
        "  dowel -C {} migrate verify <old-build>/compile_commands.json",
        source_dir.display()
    );
    Ok(())
}

fn header(system: &str) -> String {
    // Meson は翻訳の引数をリンクの引数と一緒に渡してくる（issue #135）。
    // 仕分けはこちらで行うが、落とした入力は `deps` として書き直す必要が
    // ある——読み手がそれを知らないと、下書きが繋がらない理由が分からない。
    let note = if system == "Meson" {
        "\
# Meson reports one argument list per target, mixing link inputs into it.
# Those are dropped here and listed as comments: they were the archives and
# objects this target linked, and they belong in `deps`.
#
"
    } else {
        ""
    };
    format!(
        "\
# GENERATED by `dowel migrate import` - UNVERIFIED DRAFT.
#
# This is a snapshot of one {system} configuration. Conditionals are lost, and
# the public/private intent of includes and defines is unknowable from what
# {system} reports, so everything is declared private. Promote what dependents
# need to a `public` block, then verify against the old build:
#
#   dowel migrate verify <old-build>/compile_commands.json
#
# Configuration-level flags (-O / -g / -DNDEBUG from the build type)
# were NOT copied: dowel's own debug/release configuration supplies them.
#
{note}"
    )
}

fn render_manifest(header: &str, project: &str) -> String {
    format!("{header}\n[package]\nname    = \"{}\"\nversion = \"0.0.0\"\n", sanitize(project))
}

fn render_build(header: &str, targets: &[Imported]) -> String {
    let mut out = String::from(header);
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
        // 機械が読める印を置く（[ADR-0053](../../../docs/adr/0053-unverified-import.md)）。
        // 見出しのコメントは人だけが読む。`check` が目標ごとに述べ続ける
        // ことで、残りの移植量が数えられる形になる。
        out.push_str("unverified = true\n");
        for s in &t.skipped_sources {
            out.push_str(&format!("# skipped source (outside the source tree): {s}\n"));
        }
        // 落としたリンクの入力は名前を残す。`deps` を導けない以上、
        // 何に繋がっていたかは読み手が知る必要がある（issue #135）。
        for s in &t.dropped_inputs {
            out.push_str(&format!("# link input, not a compile flag: {s} — declare it as a dep\n"));
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
    fn foreign_names_are_mapped_to_identifiers() {
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

    #[test]
    fn link_arguments_do_not_end_up_in_the_compile_flags() {
        // Meson の `parameters` にはリンクと書庫の引数が混ざる。`flags` は
        // 翻訳の引数であり、そこに入ると `cc` が入力ファイルとして読む
        // ——下書きがそのままでは組めない（issue #135）。
        let src = Path::new("/proj");
        let mut out = Imported::new("bin", "shapetool", None);
        for arg in [
            "-Wall",
            "-Wl,--as-needed",
            "-Wl,--start-group",
            "libshapes.a",
            "-Wl,--end-group",
            "-lm",
            "csrDT",
        ] {
            classify_argument(arg, src, &mut out);
        }
        assert_eq!(out.flags, ["-Wall"], "only compile flags belong in `flags`");
        assert_eq!(
            out.link_flags,
            ["-Wl,--as-needed", "-Wl,--start-group", "-Wl,--end-group", "-lm"]
        );
        // 旗でないものは落とす。書庫の名前も `ar` の引数も、翻訳には渡さない。
        assert_eq!(out.dropped_inputs, ["libshapes.a", "csrDT"]);
    }

    #[test]
    fn an_empty_include_is_not_a_directory() {
        // `-I` の引数が空のことがある。`dir("")` は何も指さない。
        let mut out = Imported::new("lib", "x", None);
        classify_argument("-I", Path::new("/proj"), &mut out);
        assert!(out.includes.is_empty() && out.external_includes.is_empty());
    }

    #[test]
    fn one_argument_list_is_sorted_into_the_right_places() {
        // Meson は仕分け済みの答を返さない。規則を1箇所に置く。
        let src = Path::new("/proj");
        let mut out = Imported::new("lib", "x", None);
        for arg in ["-I/proj/inc", "-I/usr/include", "-DLIMIT=64", "-Wall", "-O2", "-g", "-DNDEBUG"]
        {
            classify_argument(arg, src, &mut out);
        }
        assert_eq!(out.includes, ["inc"]);
        assert_eq!(out.external_includes, ["/usr/include"]);
        assert_eq!(out.defines, ["LIMIT=64"]);
        // 構成レベルのものは写さない。`-DNDEBUG` は `-D` に見えるが、
        // 供給するのは `cfg.opt` である（issue #54）。
        assert_eq!(out.flags, ["-Wall"]);
    }
}
