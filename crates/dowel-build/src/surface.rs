//! 配ったものだけで、配った面が読めるか
//! （[ADR-0060](../../../docs/adr/0060-the-surface-is-readable.md)）。
//!
//! 公開ヘッダが非公開のヘッダを `#include` していても、ビルド木の中では
//! 通る——公開と非公開の探索路が両方載っているからである。壊れるのは
//! **配った先**で、しかも壊れるのは受け取った側であり、配った側は
//! `installed:` の並びしか見ていない。[ADR-0051](../../../docs/adr/0051-source-language-is-closed.md)
//! が直した「在らないものを `built:` と言う」と同じ形である。
//!
//! 自前で `#include` を数えない。条件付き取り込みも、マクロで綴られた名前も
//! ある——C を読むのは C の道具の仕事である（ADR-0001）。**配ったものに
//! 聞く**: 配った探索路だけを与えて前処理し、通らなければ、それは受け取った
//! 側でも通らない。

use crate::toolstyle::{self, HeaderLanguage};
use dowel_eval::Config;
use dowel_support::{log_debug, Diagnostic};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 配ったヘッダ1つと、使う側がそれを読むときの条件。
///
/// 「使う側と同じに読む」ためには、道だけでは足りない。公開の語も、言語も、
/// それを配ったターゲットが決める（ADR-0060）。
#[derive(Clone, Debug)]
pub struct Header {
    /// 入れた先の道
    pub at: PathBuf,
    /// `public.includes` が書かれた位置。直す先はその行である
    pub site: Option<dowel_eval::Site>,
    /// 使う側の翻訳行に載る語。pkg-config の `Cflags` と同じものである
    pub words: Vec<String>,
    /// このヘッダを配ったターゲットが C++ を翻訳するか。
    /// `.h` をどちらの言語で読むかがこれで決まる
    pub from_cxx: bool,
}

/// ヘッダとして読む綴り。
///
/// 閉じた一覧にするのは、ソースの綴りと同じ理由である（ADR-0051）。
/// `include/` に置かれた読み物や license を前処理に掛けると、道具は綴りから
/// 言語を決められずに落ち、面の欠けと区別が付かない。
const HEADER_EXTENSIONS: &[&str] = &["h", "hh", "hpp", "hxx"];

/// 配った面が、配ったものだけで読めるか確かめる。
///
/// 道具を起動できないことは失敗ではない。検査は確信を足すものであり、
/// その不在が、それ以外は成功した install を失敗にしてはならない
/// （`exports` の検査と同じ立場、ADR-0039）。
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
        let tool = match language(&header.at, header.from_cxx) {
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
            language(&header.at, header.from_cxx),
            &header.words,
        );
        let out = match Command::new(&tool).args(&args).output() {
            Ok(o) => o,
            // 起動できないことは、面の誤りの証拠にはならない。
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
        // 道具自身の言葉を残す。欠けている名前を言うのは道具の側であり、
        // こちらが数え直すと綴りが増える。
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

/// このヘッダを読む言語。
///
/// C++ 専用の綴りは常に C++ である。`.h` は両方の言語で使われるので、それを
/// 配ったターゲットの言語に従う——`__cplusplus` の分岐がどちらへ倒れるかは、
/// そのターゲットの使い手が誰かで決まる（ADR-0060）。
fn language(path: &Path, from_cxx: bool) -> HeaderLanguage {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let cxx_only = ["hh", "hpp", "hxx"].iter().any(|h| h.eq_ignore_ascii_case(ext));
    if cxx_only || from_cxx {
        HeaderLanguage::Cxx
    } else {
        HeaderLanguage::C
    }
}

/// ヘッダとして読む綴りか。
fn is_header(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    HEADER_EXTENSIONS.iter().any(|h| h.eq_ignore_ascii_case(ext))
}

/// 道具の言葉のうち、最初の1行。
///
/// 全部を貼ると、注記が道具の出力そのものになる。読み手が要るのは
/// 「何が見つからなかったか」であり、それは最初の行に出る。
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
        // `.h` は両方の言語で使われる。`__cplusplus` の分岐がどちらへ倒れるかは
        // 綴りでは決まらない（ADR-0060）。
        assert_eq!(language(Path::new("a.h"), false), HeaderLanguage::C);
        assert_eq!(language(Path::new("a.h"), true), HeaderLanguage::Cxx);
    }

    #[test]
    fn only_the_closed_list_of_spellings_is_read() {
        for good in ["a.h", "a.hh", "a.hpp", "a.hxx", "a.H"] {
            assert!(is_header(Path::new(good)), "{good}");
        }
        // 読み物や license を前処理に掛けると、道具は綴りから言語を決められず
        // に落ち、面の欠けと区別が付かない。
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
