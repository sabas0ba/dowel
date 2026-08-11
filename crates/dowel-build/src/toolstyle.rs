//! 道具に渡す引数の綴り（[ADR-0027](../../../docs/adr/0027-toolchain-style.md)）。
//!
//! 道具の**名前**は `[toolchain]` が宣言できても、綴りが Unix 固定なら
//! `cl` は解釈できない命令が組み上がる。しかもこれらは利用者が `flags` で
//! 上書きできる旗ではない——dowel 自身が組み立てる部分だからである
//! （issue #113）。
//!
//! ここに閉じるのは**dowel が組み立てる引数だけ**である。利用者が書いた
//! `flags` は素通しする。綴りを翻訳しようとすると、どの旗をどう写すかの
//! 表を持つことになり、それは「コンパイラを知っている」ことに他ならない。
//!
//! 危ないのは `-MD` である。MSVC で `/MD` は「動的 CRT をリンクする」と
//! いう**別の、それ自体は正当な意味**を持つ。依存の書き出しを頼んだつもりの
//! 旗が ABI を選ぶ旗として解釈される——`docs/00-overview.md` が
//! 「No single ABI」の例として挙げているまさにその旗である。

use dowel_eval::config::Style;
use dowel_eval::{Config, Opt};
use std::path::Path;

/// ヘッダ依存の取り方。様式で機構ごと変わる。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Deps {
    /// コンパイラが make 形式の `.d` を書く（`-MD -MF`）
    Depfile,
    /// コンパイラが標準出力へ1行1件で並べる（`/showIncludes`）。
    /// 実行する側が拾って `.d` に畳む
    ShowIncludes,
}

/// 依存の行の接頭辞。MSVC はこれを標準出力に出す。
///
/// 既定の英語版の文言である。地域化された `cl` は別の文言を出し、ninja が
/// `msvc_deps_prefix` を持つのも同じ理由による。合わない場合に黙って依存を
/// 失うより、拾えなかったことが分かる形にしてある（[`crate::backend::direct`]）。
pub const SHOW_INCLUDES_PREFIX: &str = "Note: including file:";

/// 最適化と debug 情報の既定。
pub fn default_compile_flags(cfg: &Config) -> Vec<String> {
    match (cfg.style, cfg.opt) {
        (Style::Gnu, Opt::Debug) => vec!["-g".into(), "-O0".into()],
        (Style::Gnu, Opt::Release) => vec!["-O2".into(), "-DNDEBUG".into()],
        // `/Zi` は別ファイル（`.pdb`）に書く。`/Z7` はオブジェクトに埋める——
        // 出力が1つで済み、増分の扱いが素直なのでこちらを採る。
        (Style::Msvc, Opt::Debug) => vec!["/Z7".into(), "/Od".into()],
        (Style::Msvc, Opt::Release) => vec!["/O2".into(), "/DNDEBUG".into()],
    }
}

/// インクルード検索路。
pub fn include(cfg: &Config, path: &Path) -> String {
    match cfg.style {
        Style::Gnu => format!("-I{}", path.display()),
        Style::Msvc => format!("/I{}", path.display()),
    }
}

/// プリプロセッサ定義。
pub fn define(cfg: &Config, key: &str, value: &str) -> String {
    let body = if value.is_empty() { key.to_string() } else { format!("{key}={value}") };
    match cfg.style {
        Style::Gnu => format!("-D{body}"),
        Style::Msvc => format!("/D{body}"),
    }
}

/// 依存の取り方。
pub fn deps(cfg: &Config) -> Deps {
    match cfg.style {
        Style::Gnu => Deps::Depfile,
        Style::Msvc => Deps::ShowIncludes,
    }
}

/// 翻訳の引数のうち、入出力と依存に関わる部分。
///
/// 位置引数の順は様式が決める。GNU は `-c <入力> -o <出力>`、MSVC は
/// `/c <入力> /Fo:<出力>` である。
pub fn compile_io(cfg: &Config, src: &Path, obj: &Path, depfile: &Path) -> Vec<String> {
    match cfg.style {
        Style::Gnu => vec![
            "-MD".into(),
            "-MF".into(),
            depfile.display().to_string(),
            "-c".into(),
            src.display().to_string(),
            "-o".into(),
            obj.display().to_string(),
        ],
        Style::Msvc => vec![
            "/nologo".into(),
            "/showIncludes".into(),
            "/c".into(),
            src.display().to_string(),
            format!("/Fo:{}", obj.display()),
        ],
    }
}

/// 書庫を作る引数。
pub fn archive(cfg: &Config, out: &Path, objects: &[String]) -> Vec<String> {
    let mut args = match cfg.style {
        Style::Gnu => vec!["rcs".to_string(), out.display().to_string()],
        Style::Msvc => vec!["/nologo".to_string(), format!("/OUT:{}", out.display())],
    };
    args.extend(objects.iter().cloned());
    args
}

/// リンクの引数。`inputs` はオブジェクトと書庫を並べたもの。
///
/// 利用者の `link_flags` は綴りを変えずに置く。翻訳しようとすると、旗の
/// 対応表を持つことになる。
pub fn link(cfg: &Config, inputs: &[String], link_flags: &[String], out: &Path) -> Vec<String> {
    let mut args = Vec::new();
    if cfg.style == Style::Msvc {
        args.push("/nologo".to_string());
    }
    args.extend(inputs.iter().cloned());
    args.extend(link_flags.iter().cloned());
    match cfg.style {
        Style::Gnu => {
            args.push("-o".into());
            args.push(out.display().to_string());
        }
        // `link.exe` も `cl` も `/OUT:` を解釈する。driver が兼ねる形でも
        // 別の道具でも同じ綴りで済む。
        Style::Msvc => args.push(format!("/OUT:{}", out.display())),
    }
    args
}

/// オブジェクトファイルの拡張子。
pub fn object_extension(cfg: &Config) -> &'static str {
    match cfg.style {
        Style::Gnu => "o",
        Style::Msvc => "obj",
    }
}

/// 静的ライブラリの綴り。GNU は `lib<名前>.a`、MSVC は `<名前>.lib`。
pub fn archive_name(cfg: &Config, target: &str) -> String {
    match cfg.style {
        Style::Gnu => format!("lib{target}.a"),
        Style::Msvc => format!("{target}.lib"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(style: Style) -> Config {
        let mut c = Config::host_default();
        c.set_style(style);
        c
    }

    #[test]
    fn the_spellings_differ_where_the_two_toolchains_differ() {
        let gnu = cfg(Style::Gnu);
        let msvc = cfg(Style::Msvc);
        assert_eq!(include(&gnu, Path::new("inc")), "-Iinc");
        assert_eq!(include(&msvc, Path::new("inc")), "/Iinc");
        assert_eq!(define(&gnu, "A", "1"), "-DA=1");
        assert_eq!(define(&msvc, "A", ""), "/DA");
        assert_eq!(object_extension(&gnu), "o");
        assert_eq!(object_extension(&msvc), "obj");
        assert_eq!(archive_name(&gnu, "core"), "libcore.a");
        assert_eq!(archive_name(&msvc, "core"), "core.lib");
    }

    #[test]
    fn the_dependency_flag_that_collides_is_never_emitted_for_msvc() {
        // `/MD` は MSVC で「動的 CRT をリンクする」を意味する。依存の
        // 書き出しを頼んだ旗が ABI を選ぶ旗になってはならない（issue #113）。
        let msvc = cfg(Style::Msvc);
        let args = compile_io(&msvc, Path::new("a.c"), Path::new("a.obj"), Path::new("a.d"));
        assert!(!args.iter().any(|a| a == "-MD" || a == "/MD"), "{args:?}");
        assert!(args.iter().any(|a| a == "/showIncludes"), "{args:?}");
        assert_eq!(deps(&msvc), Deps::ShowIncludes);

        // GNU 側は従来どおり depfile を書かせる。
        let gnu = cfg(Style::Gnu);
        let args = compile_io(&gnu, Path::new("a.c"), Path::new("a.o"), Path::new("a.o.d"));
        assert!(args.iter().any(|a| a == "-MD"), "{args:?}");
        assert_eq!(deps(&gnu), Deps::Depfile);
    }

    #[test]
    fn the_output_comes_last_in_both_styles() {
        // 出力の位置は様式が決める。入力の後に置く点は共通である。
        let gnu = link(&cfg(Style::Gnu), &["a.o".into()], &["-lm".into()], Path::new("app"));
        assert_eq!(gnu, vec!["a.o", "-lm", "-o", "app"]);
        let msvc = link(
            &cfg(Style::Msvc),
            &["a.obj".into()],
            &["ws2_32.lib".into()],
            Path::new("app.exe"),
        );
        assert_eq!(msvc, vec!["/nologo", "a.obj", "ws2_32.lib", "/OUT:app.exe"]);
    }

    #[test]
    fn the_style_follows_the_triple_unless_declared() {
        use dowel_eval::config::triple_style;
        assert_eq!(triple_style("x86_64-pc-windows-msvc"), Style::Msvc);
        assert_eq!(triple_style("x86_64-pc-windows-gnu"), Style::Gnu);
        assert_eq!(triple_style("x86_64-unknown-linux-gnu"), Style::Gnu);
        // 導出された様式に応じて道具の既定も動く。
        assert_eq!(Config::for_target("x86_64-pc-windows-msvc".into()).tool("ar"), "lib");
        assert_eq!(Config::for_target("x86_64-unknown-linux-gnu".into()).tool("ar"), "ar");
    }

    #[test]
    fn the_linker_is_the_driver_unless_one_is_named() {
        // GNU では driver が兼ねる。C++ が混ざればそちらを選ぶ。
        let gnu = cfg(Style::Gnu);
        assert_eq!(gnu.linker(false), gnu.tool("c"));
        assert_eq!(gnu.linker(true), gnu.tool("cxx"));
        // MSVC では別物である。
        assert_eq!(cfg(Style::Msvc).linker(false), "link");
    }
}
