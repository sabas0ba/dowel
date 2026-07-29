//! 版の指定子。
//!
//! どの形も解決の入口にすぎず、正本は commit sha である
//! （docs/adr/0013-self-acquisition.md）。ここでは形の判定だけを行い、
//! 上流への問い合わせは `acquire` が行う。

use std::fmt;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Spec {
    /// 最新の release タグ。
    Stable,
    /// 既定ブランチの先端。
    Nightly,
    /// 既定ブランチに、その日（UTC）の終わりまでに入った最後のコミット。
    NightlyDate(String),
    /// `X.Y.Z`。タグ `vX.Y.Z` または `X.Y.Z` を指す。
    Version(String),
    /// ブランチの先端。
    Branch(String),
    /// 任意のタグ。
    Tag(String),
    /// コミット。16進の接頭辞（7桁以上）でよい。
    Sha(String),
}

pub fn parse(text: &str) -> Result<Spec, String> {
    if text == "stable" {
        return Ok(Spec::Stable);
    }
    if text == "nightly" {
        return Ok(Spec::Nightly);
    }
    if let Some(date) = text.strip_prefix("nightly-") {
        if !is_date(date) {
            return Err(format!("the date in `{text}` is not of the form nightly-YYYY-MM-DD"));
        }
        return Ok(Spec::NightlyDate(date.to_string()));
    }
    if let Some(name) = text.strip_prefix("branch:") {
        if name.is_empty() {
            return Err("`branch:` needs a branch name".to_string());
        }
        return Ok(Spec::Branch(name.to_string()));
    }
    if let Some(name) = text.strip_prefix("tag:") {
        if name.is_empty() {
            return Err("`tag:` needs a tag name".to_string());
        }
        return Ok(Spec::Tag(name.to_string()));
    }
    if is_version(text) {
        return Ok(Spec::Version(text.to_string()));
    }
    // 7桁未満の接頭辞は偶然の一致と紛れるため受け付けない。
    if (7..=40).contains(&text.len()) && is_hex(text) {
        return Ok(Spec::Sha(text.to_ascii_lowercase()));
    }
    Err(format!(
        "cannot understand the specifier `{text}`; expected stable, nightly, \
         nightly-YYYY-MM-DD, X.Y.Z, branch:<name>, tag:<name>, or a commit hash"
    ))
}

pub fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_date(date: &str) -> bool {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| date[r].bytes().all(|c| c.is_ascii_digit());
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    // 実在しない日付は git に渡す前に拒む。誤記が「解決できる別の日」に
    // 化けるのを防ぐ。
    let month: u32 = date[5..7].parse().unwrap_or(0);
    let day: u32 = date[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Spec::Stable => write!(f, "stable"),
            Spec::Nightly => write!(f, "nightly"),
            Spec::NightlyDate(d) => write!(f, "nightly-{d}"),
            Spec::Version(v) => write!(f, "{v}"),
            Spec::Branch(b) => write!(f, "branch:{b}"),
            Spec::Tag(t) => write!(f, "tag:{t}"),
            Spec::Sha(s) => write!(f, "{s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_specifier_form() {
        assert_eq!(parse("stable").unwrap(), Spec::Stable);
        assert_eq!(parse("nightly").unwrap(), Spec::Nightly);
        assert_eq!(
            parse("nightly-2026-07-29").unwrap(),
            Spec::NightlyDate("2026-07-29".to_string())
        );
        assert_eq!(parse("0.1.0").unwrap(), Spec::Version("0.1.0".to_string()));
        assert_eq!(parse("branch:feature/x").unwrap(), Spec::Branch("feature/x".to_string()));
        assert_eq!(parse("tag:snapshot-3").unwrap(), Spec::Tag("snapshot-3".to_string()));
        // sha は小文字に正規化される。
        assert_eq!(parse("2915DA5ab").unwrap(), Spec::Sha("2915da5ab".to_string()));
        // 表示は入力の形に戻る。pin のコメントと診断に使うため。
        for text in
            ["stable", "nightly", "nightly-2026-07-29", "0.1.0", "branch:x", "tag:y", "abcdef012"]
        {
            assert_eq!(parse(text).unwrap().to_string(), *text);
        }
    }

    #[test]
    fn rejects_what_cannot_be_a_specifier() {
        for text in [
            "",
            "beta",
            "nightly-2026-7-9",
            "nightly-2026-13-01",
            "branch:",
            "tag:",
            "1.2",
            "1.2.3.4",
            "abc12",
            "xyz1234",
        ] {
            assert!(parse(text).is_err(), "`{text}` should be rejected");
        }
        let too_long = "a".repeat(41);
        assert!(parse(&too_long).is_err());
    }
}
