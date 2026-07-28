//! `glob(...)` の展開。
//!
//! 評価時ではなく plan 時に行う。評価時に走査すると、その時点の
//! ファイルシステムという記録されない入力が評価結果に混ざるためである
//! （docs/00-overview.md 2節「記録されない入力を排除する」）。
//!
//! 対応する記法:
//!
//! | 記法 | 意味 |
//! |---|---|
//! | `*` | `/` を除く任意の並び |
//! | `**` | `/` を含む任意の並び |
//! | `?` | `/` を除く1文字 |

use dowel_support::{log_debug, log_trace};
use std::path::{Path, PathBuf};

/// パターンと相対パスの照合。
pub fn matches(pattern: &str, path: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    match_from(&p, 0, &s, 0)
}

fn match_from(p: &[char], mut pi: usize, s: &[char], mut si: usize) -> bool {
    while pi < p.len() {
        match p[pi] {
            '*' => {
                let double = p.get(pi + 1) == Some(&'*');
                let next = pi + if double { 2 } else { 1 };
                // 貪欲に取らず、後続が一致する最短の位置から順に試す。
                let mut k = si;
                loop {
                    if match_from(p, next, s, k) {
                        return true;
                    }
                    if k >= s.len() {
                        return false;
                    }
                    if !double && s[k] == '/' {
                        return false;
                    }
                    k += 1;
                }
            }
            '?' => {
                if si >= s.len() || s[si] == '/' {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            c => {
                if si >= s.len() || s[si] != c {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
    si == s.len()
}

/// `root` 以下を走査してパターンに一致する相対パスを返す。
///
/// 結果は辞書順に並べる。ビルドの再現性のため、走査順に依存させない。
pub fn expand(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, root, &mut found);
    let scanned = found.len();
    let mut out: Vec<PathBuf> = found
        .into_iter()
        .filter(|rel| {
            let rel = rel.to_string_lossy().replace('\\', "/");
            let hit = matches(pattern, &rel);
            // 一致しなかったものまで出す。「なぜ拾われないのか」を追うとき、
            // 走査に載ったかどうかが最初に知りたいことになる。
            log_trace!("  glob {} {rel}", if hit { "match " } else { "skip  " });
            hit
        })
        .collect();
    out.sort();
    log_debug!(
        "glob({pattern:?}) under {}: {} of {scanned} files matched",
        root.display(),
        out.len()
    );
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        let name = name.to_string_lossy();
        // 生成物と隠しディレクトリは走査しない。自分の出力を入力に取り込むと
        // ビルドのたびに結果が変わる。
        if name.starts_with('.') || name == "target" {
            log_trace!("  glob prune {}", e.path().display());
            continue;
        }
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(root, &path, out),
            Ok(_) => {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_star_does_not_cross_separators() {
        assert!(matches("src/*.c", "src/a.c"));
        assert!(!matches("src/*.c", "src/sub/a.c"));
        assert!(!matches("src/*.c", "src/a.h"));
    }

    #[test]
    fn a_double_star_crosses_separators() {
        assert!(matches("src/**.c", "src/a.c"));
        assert!(matches("src/**.c", "src/sub/deep/a.c"));
        assert!(!matches("src/**.c", "other/a.c"));
    }

    #[test]
    fn a_question_mark_matches_one_character() {
        assert!(matches("a?.c", "ab.c"));
        assert!(!matches("a?.c", "abc.c"));
        assert!(!matches("a?.c", "a/.c"));
    }

    #[test]
    fn a_bare_star_pattern() {
        assert!(matches("**", "a/b/c.c"));
        assert!(matches("*", "a.c"));
        assert!(!matches("*", "a/b.c"));
    }

    #[test]
    fn rejects_a_non_matching_suffix() {
        assert!(!matches("src/*.c", "src/a.cpp"));
        assert!(!matches("*.c", ""));
    }
}
