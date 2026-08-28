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
pub fn compile_io(cfg: &Config, src: &Path, obj: &Path, depfile: Option<&Path>) -> Vec<String> {
    match cfg.style {
        Style::Gnu => {
            let mut args = Vec::new();
            // 依存を書かせない場合がある。前処理を通らないアセンブリが
            // それで、`-MD` を渡してもコンパイラは何も書かない——宣言した
            // 出力が出ないことになる（[ADR-0048](../../../docs/adr/0048-assembly.md)）。
            if let Some(d) = depfile {
                args.push("-MD".into());
                args.push("-MF".into());
                args.push(d.display().to_string());
            }
            args.extend([
                "-c".into(),
                src.display().to_string(),
                "-o".into(),
                obj.display().to_string(),
            ]);
            args
        }
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

// --- 共有ライブラリ（ADR-0030）---

/// 書き出す形式。同じ `exports` の一覧から、リンカが読む形を作る。
///
/// 様式だけでは決まらない。GNU 様式の中で Mach-O だけが版指令書を読まず、
/// 別の綴りと別の名前の付け方を要求する——`target.os` が語彙として在る
/// （[ADR-0026](../../../docs/adr/0026-target-os-arch.md)）ので、そこで分ける。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportForm {
    /// ELF の版指令書（`--version-script`）
    VersionScript,
    /// Mach-O の記号一覧（`-exported_symbols_list`）。名前に `_` が付く
    SymbolList,
    /// Windows のモジュール定義（`/DEF:`）
    ModuleDefinition,
}

/// 形式は**対象の形式**が決める。様式ではない。
///
/// mingw は GNU 様式のまま PE を作る。版指令書は ELF のものであり、PE では
/// 意味を持たない——この2つが別の軸であることは、ここで分けておかないと
/// 「GNU 様式なら版指令書」という取り違えとして残る。
pub fn export_form(cfg: &Config) -> ExportForm {
    match (cfg.style, dowel_eval::config::triple_os(&cfg.target)) {
        (Style::Msvc, _) => ExportForm::ModuleDefinition,
        (Style::Gnu, "windows") => ExportForm::ModuleDefinition,
        (Style::Gnu, "macos") => ExportForm::SymbolList,
        (Style::Gnu, _) => ExportForm::VersionScript,
    }
}

/// 生成する記述の中身。
///
/// 記号の綴りは書かれたままにする。C++ の飾り名は利用者が書くものであり、
/// dowel は飾らない——飾り方こそ実装していない ABI そのものである
/// （ADR-0030）。Mach-O の `_` は飾りではなく platform 一律の接頭辞なので、
/// 書かれたものに付ける。
pub fn export_file(form: ExportForm, exports: &[String]) -> String {
    match form {
        ExportForm::VersionScript => {
            let mut s = String::from("{\n  global:\n");
            for name in exports {
                s.push_str(&format!("    {name};\n"));
            }
            // 挙げられていないものは局所。既定で全部出る側を、
            // 挙げた分だけに閉じるのがこの生成の目的である。
            s.push_str("  local:\n    *;\n};\n");
            s
        }
        ExportForm::SymbolList => {
            let mut s = String::new();
            for name in exports {
                s.push_str(&format!("_{name}\n"));
            }
            s
        }
        ExportForm::ModuleDefinition => {
            let mut s = String::from("EXPORTS\n");
            for name in exports {
                s.push_str(&format!("    {name}\n"));
            }
            s
        }
    }
}

/// 生成する記述のファイル名の末尾。
pub fn export_file_extension(form: ExportForm) -> &'static str {
    match form {
        ExportForm::VersionScript => "map",
        ExportForm::SymbolList => "symbols",
        ExportForm::ModuleDefinition => "def",
    }
}

/// アセンブリを組み立てるときの追加（ADR-0048）。
///
/// `.note.GNU-stack` を持たない目的ファイルは、リンカに「実行可能スタックが
/// 要る」と読まれる。C のコンパイラは自分の出力に印を付けるが、手書きの
/// アセンブリには誰も付けない——`-fPIC` と同じで、出来上がるものの正しさが
/// 掛かっている引数なので dowel が組み立てる（ADR-0030 と同じ判断）。
///
/// 本当に実行可能スタックが要る稀な場合は `asm_flags` で言い直せる。
pub fn assemble_flags(cfg: &Config) -> Vec<String> {
    match cfg.style {
        Style::Gnu => vec!["-Wa,--noexecstack".into()],
        // MSVC の `ml` は別の道具であり、この印は ELF のものである。
        Style::Msvc => Vec::new(),
    }
}

/// 実行可能スタックを断るリンク時の引数（ADR-0050）。
///
/// 別のアセンブラが組み立てた目的ファイルに `.note.GNU-stack` は無く、
/// dowel はその道具の綴りで印を頼めない。リンカの綴りは様式が決めるので
/// 知っている——同じ主張を、まだ言える場所で言う。
/// `link_flags` はこの後に並ぶので、本当に要る稀な場合は `-z execstack` で
/// 言い直せる。
pub fn noexecstack_link_flags(cfg: &Config) -> Vec<String> {
    match cfg.style {
        Style::Gnu => vec!["-z".into(), "noexecstack".into()],
        // 実行可能スタックは ELF の話である。
        Style::Msvc => Vec::new(),
    }
}

/// 別に宣言されたアセンブラに渡す入出力
/// （[ADR-0050](../../../docs/adr/0050-separate-assembler.md)）。
///
/// dowel が組み立てるのはこれだけである。翻訳の行の残り——最適化、`flags`、
/// インクルード検索路、定義——は C の driver の綴りであって、アセンブラは
/// driver ではない。それらを言い直す場所は `asm_flags` であり、
/// `List<Word>` なので `["-I", dir("asm")]` のようにパスも書ける。
pub fn assemble_io(cfg: &Config, src: &Path, obj: &Path) -> Vec<String> {
    match cfg.style {
        Style::Gnu => {
            vec!["-o".into(), obj.display().to_string(), src.display().to_string()]
        }
        // `ml64` は翻訳だけの指定を要し、出力の綴りが `/Fo<ファイル>` である。
        Style::Msvc => vec![
            "/nologo".into(),
            "/c".into(),
            format!("/Fo{}", obj.display()),
            src.display().to_string(),
        ],
    }
}

/// 共有ライブラリに入りうるオブジェクトの翻訳時の追加。
///
/// 位置独立にするだけである。`-fvisibility=hidden` は**足さない**——
/// 翻訳時に隠された記号は版指令書の `global:` では戻らず、隠す指定と
/// 挙げる指定を併せると何も出ない共有ライブラリが出来る（ADR-0030）。
/// 隠すのは指令書の役目であり、指令書だけで足りる。
pub fn shared_object_flags(cfg: &Config) -> Vec<String> {
    match cfg.style {
        Style::Gnu => vec!["-fPIC".into()],
        // MSVC の目的コードは元より位置独立であり、既定で何も出さない。
        Style::Msvc => Vec::new(),
    }
}

/// 共有ライブラリの綴り。版を書けば、それが名前に入る
/// （[ADR-0040](../../../docs/adr/0040-shared-library-version.md)）。
///
/// 版の入る位置は形式ごとに違う。ELF は末尾に足し、Mach-O は拡張子の前に
/// 挟み、PE は名前そのものに繋ぐ。どれも各々の慣習であって、選べるもので
/// はない——利用者が置き場所を見て名前を読むためである。
pub fn shared_library_name(cfg: &Config, target: &str, soversion: Option<i64>) -> String {
    let plain = shared_library_link_name(cfg, target);
    let Some(v) = soversion else { return plain };
    match (cfg.style, dowel_eval::config::triple_os(&cfg.target)) {
        (Style::Msvc, _) => format!("{target}-{v}.dll"),
        (Style::Gnu, "macos") => format!("lib{target}.{v}.dylib"),
        (Style::Gnu, "windows") => format!("lib{target}-{v}.dll"),
        (Style::Gnu, _) => format!("lib{target}.so.{v}"),
    }
}

/// 版を持たない綴り。`-lcore` が見つける名前である。
pub fn shared_library_link_name(cfg: &Config, target: &str) -> String {
    match (cfg.style, dowel_eval::config::triple_os(&cfg.target)) {
        (Style::Msvc, _) => format!("{target}.dll"),
        (Style::Gnu, "macos") => format!("lib{target}.dylib"),
        // mingw も GNU 様式だが、出来上がるのは DLL である。
        (Style::Gnu, "windows") => format!("lib{target}.dll"),
        (Style::Gnu, _) => format!("lib{target}.so"),
    }
}

/// 版付きの実体に対し、版を持たない名前を並べて置くか。
///
/// 置くのは記号連結を使う形式に限る。PE には記号連結が無く、`-lcore` に
/// 当たるのも DLL ではなく取り込み用の書庫であるため、並べる意味が無い。
pub fn has_link_name_alias(cfg: &Config) -> bool {
    !matches!(export_form(cfg), ExportForm::ModuleDefinition)
}

/// 共有ライブラリを作るリンクの引数。
///
/// `soname` を付けるのは、付けないと依存側がリンク時のパスをそのまま
/// 記録し、rpath が無意味になるためである（ADR-0030）。
pub fn link_shared(
    cfg: &Config,
    inputs: &[String],
    link_flags: &[String],
    out: &Path,
    export_file: &Path,
) -> Vec<String> {
    let name = out.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut args = Vec::new();
    let mut trailing_inputs: Vec<String> = Vec::new();
    // 様式が綴りを決め、形式が記述の渡し方を決める。mingw のように
    // 「GNU 様式で PE」の組み合わせがあるため、2つを別に見る。
    match cfg.style {
        Style::Gnu => {
            match export_form(cfg) {
                ExportForm::VersionScript => {
                    args.push("-shared".into());
                    args.push(format!("-Wl,-soname,{name}"));
                    args.push(format!("-Wl,--version-script={}", export_file.display()));
                }
                ExportForm::SymbolList => {
                    args.push("-dynamiclib".into());
                    args.push(format!("-Wl,-install_name,@rpath/{name}"));
                    args.push("-Wl,-exported_symbols_list".into());
                    args.push(export_file.display().to_string());
                }
                // PE では記述を入力ファイルとして置く。soname は無い。
                ExportForm::ModuleDefinition => {
                    args.push("-shared".into());
                    trailing_inputs.push(export_file.display().to_string());
                }
            }
        }
        Style::Msvc => {
            args.push("/nologo".into());
            args.push("/DLL".into());
            args.push(format!("/DEF:{}", export_file.display()));
        }
    }
    args.extend(inputs.iter().cloned());
    args.extend(trailing_inputs);
    args.extend(link_flags.iter().cloned());
    match cfg.style {
        Style::Gnu => {
            args.push("-o".into());
            args.push(out.display().to_string());
        }
        Style::Msvc => args.push(format!("/OUT:{}", out.display())),
    }
    args
}

/// 出来上がった共有ライブラリに「何を書き出したか」を聞く引数
/// （[ADR-0039](../../../docs/adr/0039-exports-are-checked.md)）。
///
/// ヘッダを読む言語（[ADR-0060](../../../docs/adr/0060-the-surface-is-readable.md)）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderLanguage {
    C,
    Cxx,
}

/// 前処理だけ行う綴り（ADR-0060）。
///
/// 翻訳はしない。確かめたいのは「読めるか」——`#include` が届くか——であり、
/// 型の整合は別の問いである。出力は捨てる。
///
/// **言語は明示する。** 綴りから決めさせると、driver の知らない綴り
/// （`.HH` など）は「リンクの入力」として読まれずに終わり、警告と終了状態 0
/// が返る——[ADR-0051](../../../docs/adr/0051-source-language-is-closed.md)
/// が直したのと同じ、読まれないまま成功する形である。
///
/// **公開の語も渡す。** `public.defines` と `public.flags` は pkg-config の
/// `Cflags` に載る（[ADR-0043](../../../docs/adr/0043-pkgconfig-generation.md)）。
/// 使う側がそれを持って読む以上、こちらも持たなければ、定義で開く
/// `#include` を見落とす。
pub fn preprocess_only(
    cfg: &Config,
    include_dir: &Path,
    header: &Path,
    language: HeaderLanguage,
    words: &[String],
) -> Vec<String> {
    let mut args = match cfg.style {
        Style::Gnu => vec![
            "-x".into(),
            match language {
                HeaderLanguage::C => "c-header".into(),
                HeaderLanguage::Cxx => "c++-header".into(),
            },
            "-E".into(),
            "-o".into(),
            devnull(),
        ],
        // `/E` は標準出力へ出す。`/TC` / `/TP` は「すべてを C（C++）として
        // 扱う」であり、綴りから言語を決めさせないためにどちらかを必ず置く。
        Style::Msvc => vec![
            "/nologo".into(),
            "/E".into(),
            match language {
                HeaderLanguage::C => "/TC".into(),
                HeaderLanguage::Cxx => "/TP".into(),
            },
        ],
    };
    args.push(include(cfg, include_dir));
    args.extend(words.iter().cloned());
    args.push(header.display().to_string());
    args
}

/// 捨て場の名前。
fn devnull() -> String {
    if cfg!(windows) {
        "NUL".to_string()
    } else {
        "/dev/null".to_string()
    }
}

/// 読むのは道具の出力であって、目的ファイルではない。形式の解読は道具の
/// 側に残る（[ADR-0001](../../../docs/adr/0001-toolchain-vs-supply.md)）。
pub fn list_exports(cfg: &Config, library: &Path) -> Vec<String> {
    match cfg.style {
        // `-D` は動的記号表、`--defined-only` はこのファイルが定義したもの。
        Style::Gnu => {
            vec!["-D".into(), "--defined-only".into(), library.display().to_string()]
        }
        Style::Msvc => vec!["/nologo".into(), "/exports".into(), library.display().to_string()],
    }
}

/// 道具の出力から記号の名前だけを拾う。
///
/// 綴りは様式で違う。GNU の `nm` は `<番地> <種別> <名前>`、MSVC の
/// `dumpbin /exports` は見出しと表の後に `<序数> <hint> <RVA> <名前>` を
/// 並べる。どちらも「行の最後の語」が名前であることを使う——書式の全体を
/// 解釈すると、道具の版ごとの差に付き合うことになる。
pub fn parse_exports(cfg: &Config, output: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let name = match cfg.style {
            // `0000000000001139 T core_open`。種別が1文字の行だけを採る。
            Style::Gnu => {
                if fields.len() != 3 || fields[1].len() != 1 {
                    continue;
                }
                fields[2]
            }
            // `1    0 00001000 core_open`。4語で、先頭3つが数字の行。
            Style::Msvc => {
                if fields.len() != 4 || !fields[0].bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                fields[3]
            }
        };
        if !name.is_empty() && !out.contains(&name.to_string()) {
            out.push(name.to_string());
        }
    }
    out
}

/// 共有ライブラリに繋ぐ側が、実行時にそれを見つけるための引数。
///
/// Windows には rpath が無い。実行する側が環境で渡す（ADR-0030）。
pub fn runtime_search_path(cfg: &Config, dir: &Path) -> Vec<String> {
    match dowel_eval::config::triple_os(&cfg.target) {
        "windows" => Vec::new(),
        _ => vec![format!("-Wl,-rpath,{}", dir.display())],
    }
}

/// 同じ探索路を、**自分自身からの相対**で言い直したもの
/// （[ADR-0041](../../../docs/adr/0041-install.md)）。
///
/// ビルドディレクトリの絶対パスだけを記録した実行ファイルは、置き場所を
/// 移した瞬間にライブラリを見失う。しかもビルド木が在る限り動いてしまう
/// ので、壊れているのは配った先で分かる。
///
/// `rel` は、実行ファイルの置かれる場所から共有ライブラリの場所への相対で
/// ある——`bin/` から見れば `../lib`、`lib/` の中同士なら `.`。
pub fn relocatable_search_path(cfg: &Config, rel: &str) -> Vec<String> {
    match dowel_eval::config::triple_os(&cfg.target) {
        "windows" => Vec::new(),
        // Mach-O の綴りは別である。`$ORIGIN` は解釈されない。
        "macos" => vec![format!("-Wl,-rpath,@loader_path/{rel}")],
        _ => vec![format!("-Wl,-rpath,$ORIGIN/{rel}")],
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
        let args = compile_io(&msvc, Path::new("a.c"), Path::new("a.obj"), Some(Path::new("a.d")));
        assert!(!args.iter().any(|a| a == "-MD" || a == "/MD"), "{args:?}");
        assert!(args.iter().any(|a| a == "/showIncludes"), "{args:?}");
        assert_eq!(deps(&msvc), Deps::ShowIncludes);

        // GNU 側は従来どおり depfile を書かせる。
        let gnu = cfg(Style::Gnu);
        let args = compile_io(&gnu, Path::new("a.c"), Path::new("a.o"), Some(Path::new("a.o.d")));
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

    fn for_target(triple: &str) -> Config {
        Config::for_target(triple.into())
    }

    #[test]
    fn the_export_form_follows_the_object_format_not_the_argument_style() {
        // mingw は GNU 様式のまま PE を作る。版指令書は ELF のものであり、
        // 「GNU 様式なら版指令書」と決めると PE で意味を失う。
        assert_eq!(export_form(&for_target("x86_64-unknown-linux-gnu")), ExportForm::VersionScript);
        assert_eq!(export_form(&for_target("aarch64-apple-darwin")), ExportForm::SymbolList);
        assert_eq!(export_form(&for_target("x86_64-pc-windows-gnu")), ExportForm::ModuleDefinition);
        assert_eq!(
            export_form(&for_target("x86_64-pc-windows-msvc")),
            ExportForm::ModuleDefinition
        );
    }

    #[test]
    fn one_export_list_becomes_each_linkers_own_form() {
        let exports = vec!["core_open".to_string(), "core_close".to_string()];

        let map = export_file(ExportForm::VersionScript, &exports);
        assert!(map.contains("core_open;"), "{map}");
        // 挙げていないものが閉じることが目的である。
        assert!(map.contains("local:") && map.contains("*;"), "{map}");

        // Mach-O は platform 一律の接頭辞を要求する。飾り名ではない。
        assert_eq!(export_file(ExportForm::SymbolList, &exports), "_core_open\n_core_close\n");

        let def = export_file(ExportForm::ModuleDefinition, &exports);
        assert!(def.starts_with("EXPORTS\n"), "{def}");
        assert!(def.contains("core_open"), "{def}");
    }

    #[test]
    fn a_shared_library_is_spelled_and_named_per_target() {
        let exports = Path::new("core.map");
        let linux = for_target("x86_64-unknown-linux-gnu");
        assert_eq!(shared_library_name(&linux, "core", None), "libcore.so");
        let args =
            link_shared(&linux, &["a.o".into()], &[], Path::new("/b/lib/libcore.so"), exports);
        assert!(args.contains(&"-shared".to_string()), "{args:?}");
        // soname が無いと、依存側はリンク時のパスを記録し rpath が効かない。
        assert!(args.contains(&"-Wl,-soname,libcore.so".to_string()), "{args:?}");
        assert!(args.iter().any(|a| a.starts_with("-Wl,--version-script=")), "{args:?}");

        let mac = for_target("aarch64-apple-darwin");
        assert_eq!(shared_library_name(&mac, "core", None), "libcore.dylib");
        let args = link_shared(&mac, &["a.o".into()], &[], Path::new("/b/libcore.dylib"), exports);
        assert!(args.contains(&"-dynamiclib".to_string()), "{args:?}");
        assert!(args.contains(&"-Wl,-install_name,@rpath/libcore.dylib".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a.contains("version-script")), "{args:?}");

        let msvc = for_target("x86_64-pc-windows-msvc");
        assert_eq!(shared_library_name(&msvc, "core", None), "core.dll");
        let args = link_shared(&msvc, &["a.obj".into()], &[], Path::new("/b/core.dll"), exports);
        assert!(args.contains(&"/DLL".to_string()), "{args:?}");
        assert!(args.iter().any(|a| a.starts_with("/DEF:")), "{args:?}");
        assert!(!args.contains(&"-shared".to_string()), "{args:?}");

        // mingw: GNU の綴りで、記述は入力ファイルとして置く。
        let mingw = for_target("x86_64-pc-windows-gnu");
        assert_eq!(shared_library_name(&mingw, "core", None), "libcore.dll");
        let args = link_shared(
            &mingw,
            &["a.o".into()],
            &[],
            Path::new("/b/libcore.dll"),
            Path::new("c.def"),
        );
        assert!(args.contains(&"-shared".to_string()), "{args:?}");
        assert!(args.contains(&"c.def".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a.contains("soname")), "{args:?}");
    }

    #[test]
    fn a_declared_version_enters_the_name_where_the_format_puts_it() {
        // 位置は形式ごとの慣習であり、選べるものではない（ADR-0040）。
        let linux = for_target("x86_64-unknown-linux-gnu");
        assert_eq!(shared_library_name(&linux, "core", Some(1)), "libcore.so.1");
        let mac = for_target("aarch64-apple-darwin");
        assert_eq!(shared_library_name(&mac, "core", Some(1)), "libcore.1.dylib");
        let msvc = for_target("x86_64-pc-windows-msvc");
        assert_eq!(shared_library_name(&msvc, "core", Some(1)), "core-1.dll");
        let mingw = for_target("x86_64-pc-windows-gnu");
        assert_eq!(shared_library_name(&mingw, "core", Some(1)), "libcore-1.dll");

        // soname は出力の名前から取るので、版はそのまま入る。付けないと
        // 使う側はリンク時のパスを記録し、世代を跨いだ入れ替えが効かない。
        let args = link_shared(
            &linux,
            &["a.o".into()],
            &[],
            Path::new("/b/lib/libcore.so.1"),
            Path::new("core.map"),
        );
        assert!(args.contains(&"-Wl,-soname,libcore.so.1".to_string()), "{args:?}");

        // 版を持たない名前を並べるのは記号連結を使う形式に限る。PE には
        // 無く、`-lcore` が当たるのも DLL ではなく取り込み用の書庫である。
        assert!(has_link_name_alias(&linux));
        assert!(has_link_name_alias(&mac));
        assert!(!has_link_name_alias(&msvc));
        assert!(!has_link_name_alias(&mingw));
        assert_eq!(shared_library_link_name(&linux, "core"), "libcore.so");
    }

    #[test]
    fn assembly_gets_the_stack_note_and_can_skip_the_depfile() {
        // 手書きのアセンブリには誰も `.note.GNU-stack` を付けない。
        // 付かない目的ファイルは「実行可能スタックが要る」と読まれる
        // （ADR-0048）。
        let gnu = for_target("x86_64-unknown-linux-gnu");
        assert_eq!(assemble_flags(&gnu), vec!["-Wa,--noexecstack"]);
        // この印は ELF のものである。MSVC の `ml` は別の道具である。
        assert!(assemble_flags(&for_target("x86_64-pc-windows-msvc")).is_empty());

        // 依存を頼まない形が要る。前処理を通らない `.s` に依存は無く、
        // `-MD` を渡してもコンパイラは何も書かない。
        let args = compile_io(&gnu, Path::new("a.s"), Path::new("a.o"), None);
        assert!(!args.iter().any(|a| a == "-MD"), "{args:?}");
        assert!(args.contains(&"-c".to_string()), "{args:?}");
    }

    #[test]
    fn the_objects_are_position_independent_but_not_hidden() {
        let gnu = shared_object_flags(&for_target("x86_64-unknown-linux-gnu"));
        assert!(gnu.contains(&"-fPIC".to_string()), "{gnu:?}");
        // 翻訳時に隠すと版指令書の `global:` では戻らず、挙げた記号まで
        // 出なくなる。隠すのは指令書の役目である（ADR-0030）。
        assert!(!gnu.iter().any(|f| f.contains("visibility")), "{gnu:?}");
        // MSVC の目的コードは元より位置独立で、既定で何も出さない。
        assert!(shared_object_flags(&for_target("x86_64-pc-windows-msvc")).is_empty());

        let dir = Path::new("/b/lib");
        assert_eq!(
            runtime_search_path(&for_target("x86_64-unknown-linux-gnu"), dir),
            vec!["-Wl,-rpath,/b/lib"]
        );
        // Windows に rpath は無い。実行する側が環境で渡す。
        assert!(runtime_search_path(&for_target("x86_64-pc-windows-msvc"), dir).is_empty());

        // 同じ探索路を自分自身からの相対で言い直したもの（ADR-0041）。
        // 綴りは形式ごとに違う——Mach-O は `$ORIGIN` を解釈しない。
        assert_eq!(
            relocatable_search_path(&for_target("x86_64-unknown-linux-gnu"), "../lib"),
            vec!["-Wl,-rpath,$ORIGIN/../lib"]
        );
        assert_eq!(
            relocatable_search_path(&for_target("aarch64-apple-darwin"), "."),
            vec!["-Wl,-rpath,@loader_path/."]
        );
        assert!(relocatable_search_path(&for_target("x86_64-pc-windows-msvc"), "../lib").is_empty());
        assert!(runtime_search_path(&for_target("x86_64-pc-windows-gnu"), dir).is_empty());
    }

    #[test]
    fn the_symbol_listers_output_is_read_per_style() {
        // 読むのは道具の出力であって目的ファイルではない（ADR-0039）。
        // 書式の全体は解釈しない——道具の版ごとの差に付き合うことになる。
        let gnu = cfg(Style::Gnu);
        let listed = parse_exports(
            &gnu,
            "0000000000001139 T core_open\n\
             0000000000001150 T core_close\n\
                              U printf\n",
        );
        // 定義しているものだけ。未定義（`U`）は3語に満たない。
        assert_eq!(listed, ["core_open", "core_close"]);

        let msvc = cfg(Style::Msvc);
        let listed = parse_exports(
            &msvc,
            "Dump of file core.dll\n\n\
             ordinal hint RVA      name\n\n\
             1    0 00001000 core_open\n\
             2    1 00001010 core_close\n",
        );
        assert_eq!(listed, ["core_open", "core_close"]);

        // 引数の綴りも様式が決める。
        assert!(list_exports(&gnu, Path::new("l.so")).contains(&"--defined-only".to_string()));
        assert!(list_exports(&msvc, Path::new("l.dll")).contains(&"/exports".to_string()));
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
