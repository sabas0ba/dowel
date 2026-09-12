//! インストール済み公開ヘッダの前処理検証
//! （[ADR-0060](../../../docs/adr/0060-the-surface-is-readable.md)）。
//!
//! ビルド時には公開・非公開の両方のインクルード検索パスを使用するため、
//! 公開ヘッダが非公開ヘッダに依存していてもコンパイルできる。
//! インストール後にその非公開ヘッダが見つからない問題を、提供側で検出する。
//!
//! 条件付きの `#include` やマクロで指定されたファイル名を扱うため、
//! 検査にはコンパイラのプリプロセッサを使用する（ADR-0001）。
//! インストール先の検索パスと利用側に伝播するコンパイル引数で前処理する。

use crate::toolstyle::{self, HeaderLanguage};
use dowel_eval::Config;
use dowel_support::{log_debug, Diagnostic};
use std::path::{Path, PathBuf};
use std::process::Command;

/// インストール済みヘッダと、利用側の前処理条件。
///
/// 検索パスに加え、提供元ターゲットの言語と公開コンパイル引数を使用する（ADR-0060）。
#[derive(Clone, Debug)]
pub struct Header {
    /// インストール先のファイルパス
    pub at: PathBuf,
    /// `public.includes` が書かれた位置。直す先はその行である
    pub site: Option<dowel_eval::Site>,
    /// 前処理の言語。使用するコンパイラと、言語別の引数を決める
    pub language: HeaderLanguage,
    /// 利用側に伝播するコンパイル引数。対象言語の引数だけを含む
    pub words: Vec<String>,
}

/// 前処理検査の対象とするヘッダの拡張子。
///
/// 対応する拡張子を列挙し、README やライセンス文書を検査対象から除外する。
/// 文書の前処理エラーを、公開ヘッダの依存不足として報告しないためである（ADR-0051）。
const HEADER_EXTENSIONS: &[&str] = &["h", "hh", "hpp", "hxx"];

/// インストール済み公開ヘッダを前処理し、失敗したヘッダの警告を返す。
///
/// コンパイラが見つからない場合はそのヘッダをスキップし、起動エラー時は検査を終了する。
/// 検査を実行できないことを理由に install を失敗させない（ADR-0039）。
pub fn check(headers: &[Header], include_root: &Path, cfg: &Config) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let readable: Vec<&Header> = headers.iter().filter(|h| is_header(&h.at)).collect();
    if readable.is_empty() {
        return diags;
    }
    for header in readable {
        if !header.at.is_file() {
            continue;
        }
        // 読む道具も言語で選ぶ。C++ のヘッダを C の driver へ渡すと、標準
        // ライブラリの探索路が揃わない。
        let tool = match header.language {
            HeaderLanguage::C => cfg.tool("c").to_string(),
            HeaderLanguage::Cxx => cfg.tool("cxx").to_string(),
        };
        if !crate::exec::program_exists(&tool) {
            log_debug!("surface: `{tool}` is not on PATH; not reading {}", header.at.display());
            continue;
        }
        let args = toolstyle::preprocess_only(
            cfg,
            include_root,
            &header.at,
            header.language,
            &header.words,
        );
        let out = match Command::new(&tool).args(&args).output() {
            Ok(o) => o,
            // 起動エラーだけでは公開ヘッダに問題があると判断できない。
            Err(e) => {
                log_debug!("surface: cannot start `{tool}`: {e}");
                return diags;
            }
        };
        if out.status.success() {
            continue;
        }
        let said = String::from_utf8_lossy(&out.stderr);
        let name =
            header.at.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut d = Diagnostic::warning(
            "unreadable-surface",
            format!("`{name}` cannot be read from what was installed"),
        );
        if let Some(s) = header.site {
            d = d.at(s.file, s.span, "this is what a consumer compiles against");
        }
        // 不足しているヘッダ名などを含むコンパイラの診断を、そのまま注記に使用する。
        if let Some(line) = first_complaint(&said) {
            d = d.note(line);
        }
        diags.push(
            d.note(format!(
                "preprocessed with `{tool}` against `{}` alone, the way a consumer does",
                include_root.display()
            ))
            .note("a header the surface reaches has to be installed too, or moved out of it"),
        );
    }
    diags
}

/// ヘッダの前処理に使用する言語。
///
/// `.hh`、`.hpp`、`.hxx` は C++ として扱う。`.h` は提供元ターゲットが
/// C++ を使用する場合に C++ として前処理し、`__cplusplus` の条件を合わせる（ADR-0060）。
pub fn language(path: &Path, from_cxx: bool) -> HeaderLanguage {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let cxx_only = ["hh", "hpp", "hxx"].iter().any(|h| h.eq_ignore_ascii_case(ext));
    if cxx_only || from_cxx {
        HeaderLanguage::Cxx
    } else {
        HeaderLanguage::C
    }
}

/// 前処理検査の対象拡張子か。大文字・小文字は区別しない。
pub fn is_header(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    HEADER_EXTENSIONS.iter().any(|h| h.eq_ignore_ascii_case(ext))
}

/// コンパイラの標準エラー出力から、最初の空でない行を取得する。
///
/// 診断の注記を短く保つため、後続のソース抜粋や終了メッセージは含めない。
fn first_complaint(said: &str) -> Option<String> {
    said.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cxx_spelling_is_read_as_cxx_whatever_the_target_compiles() {
        assert_eq!(language(Path::new("a.hpp"), false), HeaderLanguage::Cxx);
        assert_eq!(language(Path::new("a.HXX"), false), HeaderLanguage::Cxx);
    }

    #[test]
    fn a_plain_header_follows_the_target_that_shipped_it() {
        // `.h` だけでは言語を判定できないため、提供元ターゲットの言語を使う（ADR-0060）。
        assert_eq!(language(Path::new("a.h"), false), HeaderLanguage::C);
        assert_eq!(language(Path::new("a.h"), true), HeaderLanguage::Cxx);
    }

    #[test]
    fn only_the_closed_list_of_spellings_is_read() {
        for good in ["a.h", "a.hh", "a.hpp", "a.hxx", "a.H"] {
            assert!(is_header(Path::new(good)), "{good}");
        }
        // 文書やソースファイルを、公開ヘッダの前処理検査に含めない。
        for other in ["README", "notes.txt", "a.c", "a.cpp"] {
            assert!(!is_header(Path::new(other)), "{other}");
        }
    }

    #[test]
    fn the_first_thing_the_tool_said_is_what_is_shown() {
        let said = "\n  core.h:1:10: fatal error: core_types.h: No such file or directory\n  1 | #include\ncompilation terminated.\n";
        assert_eq!(
            first_complaint(said).as_deref(),
            Some("core.h:1:10: fatal error: core_types.h: No such file or directory")
        );
        assert_eq!(first_complaint("   \n\n"), None);
    }
}
