//! 移行の等価性検査（`dowel migrate verify`、docs/40-migration.md 4節）。
//!
//! 既存ビルドシステムが出した `compile_commands.json` を正とし、dowel の
//! 計画が同じソースへ同じコンパイル引数を与えるかを比べる。移行を一度きりの
//! 変換ではなく、段階的移植中の**継続的な等価性検査**として提供する。
//!
//! ## 比較の正規化
//!
//! 生の引数列は同値でも一致しない（コンパイラのパス、`-o` の出力先、
//! depfile の指定、`-I` の相対・絶対）。意味に効く部分へ落としてから比べる。
//!
//! - `-D` は `NAME` を `NAME=1` に正規化して集合で比べる（プリプロセッサの意味論）
//! - `-I` / `-isystem` は各エントリの `directory` 基準で絶対化して集合で比べる
//! - `-c` / `-o` / depfile 生成（`-MD` 系）/ ソース自身 / コンパイラ名は除く
//! - **構成のフラグ**（最適化・デバッグ情報・`NDEBUG`、[`is_config_flag`]）は
//!   両側から等しく除く。これらは dowel では `cfg.opt` が、参照側では
//!   build type が供給するもので、突き合わせる相手が違う。比較に持ち込むと
//!   移行の成否と無関係な理由で全ソースが `differing` になる（issue #54）
//! - 残りのフラグは多重集合で比べる。順序は比べない（順序が意味を持つ
//!   フラグ列は稀であり、並び替えの差で埋もれる方が害が大きい）

use crate::plan::Plan;
use dowel_support::json::{self, Json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// 参照側（既存システム）の1エントリ。
pub struct RefEntry {
    pub file: PathBuf,
    norm: Normalized,
}

/// 意味に効く部分へ正規化したコンパイル引数。
#[derive(Default, PartialEq, Eq)]
struct Normalized {
    defines: BTreeSet<String>,
    includes: BTreeSet<PathBuf>,
    /// その他のフラグ → 出現回数
    flags: BTreeMap<String, usize>,
}

/// 1ソースの差分。`missing` は参照側にだけあるもの、`extra` は dowel 側にだけあるもの。
pub struct SourceDiff {
    pub file: PathBuf,
    pub missing: Vec<String>,
    pub extra: Vec<String>,
}

pub struct Verdict {
    /// 参照と一致したソースの数
    pub equivalent: usize,
    pub differing: Vec<SourceDiff>,
    /// 参照側にだけあるソース（未移植）
    pub unported: Vec<PathBuf>,
    /// dowel 側にだけあるソース（テスト等。失敗にはしない）
    pub extra_sources: Vec<PathBuf>,
}

impl Verdict {
    /// 移植済みの範囲が等価か。未移植は段階的移植の途中経過であり、失敗にしない。
    pub fn ported_sources_are_equivalent(&self) -> bool {
        self.differing.is_empty()
    }
}

/// `compile_commands.json` の本文を読む。
pub fn read_reference(text: &str) -> Result<Vec<RefEntry>, String> {
    let root = json::parse(text).ok_or("the reference is not valid JSON")?;
    let entries = root.as_array().ok_or("the reference is not a JSON array")?;
    let mut out = Vec::new();
    for e in entries {
        let Some(file) = e.get("file").and_then(Json::as_str) else { continue };
        let Some(dir) = e.get("directory").and_then(Json::as_str) else { continue };
        let dir = PathBuf::from(dir);
        let args: Vec<String> = if let Some(list) = e.get("arguments").and_then(Json::as_array) {
            list.iter().filter_map(Json::as_str).map(str::to_string).collect()
        } else if let Some(cmd) = e.get("command").and_then(Json::as_str) {
            split_command(cmd)
        } else {
            continue;
        };
        let file = absolute(&dir, Path::new(file));
        let norm = normalize(&args, &dir, &file);
        out.push(RefEntry { file, norm });
    }
    if out.is_empty() {
        return Err("the reference contains no usable entries".into());
    }
    Ok(out)
}

/// dowel の計画と参照を比べる。
pub fn compare(plan: &Plan, reference: &[RefEntry]) -> Verdict {
    let mut ours: BTreeMap<PathBuf, Normalized> = BTreeMap::new();
    for cc in &plan.compile_commands {
        let file = absolute(&cc.directory, &cc.file);
        let norm = normalize(&cc.arguments, &cc.directory, &file);
        ours.insert(file, norm);
    }

    let mut verdict = Verdict {
        equivalent: 0,
        differing: Vec::new(),
        unported: Vec::new(),
        extra_sources: Vec::new(),
    };
    let mut seen: BTreeSet<&Path> = BTreeSet::new();
    for r in reference {
        seen.insert(&r.file);
        match ours.get(&r.file) {
            None => verdict.unported.push(r.file.clone()),
            Some(mine) if *mine == r.norm => verdict.equivalent += 1,
            Some(mine) => verdict.differing.push(diff(&r.file, &r.norm, mine)),
        }
    }
    for file in ours.keys() {
        if !seen.contains(file.as_path()) {
            verdict.extra_sources.push(file.clone());
        }
    }
    verdict
}

fn diff(file: &Path, theirs: &Normalized, ours: &Normalized) -> SourceDiff {
    let mut missing = Vec::new();
    let mut extra = Vec::new();
    for d in theirs.defines.difference(&ours.defines) {
        missing.push(format!("-D{d}"));
    }
    for d in ours.defines.difference(&theirs.defines) {
        extra.push(format!("-D{d}"));
    }
    for i in theirs.includes.difference(&ours.includes) {
        missing.push(format!("-I{}", i.display()));
    }
    for i in ours.includes.difference(&theirs.includes) {
        extra.push(format!("-I{}", i.display()));
    }
    for (f, n) in &theirs.flags {
        let have = ours.flags.get(f).copied().unwrap_or(0);
        for _ in have..*n {
            missing.push(f.clone());
        }
    }
    for (f, n) in &ours.flags {
        let have = theirs.flags.get(f).copied().unwrap_or(0);
        for _ in have..*n {
            extra.push(f.clone());
        }
    }
    SourceDiff { file: file.to_path_buf(), missing, extra }
}

/// 構成（build type / `cfg.opt`）が供給するフラグか。
///
/// 最適化・デバッグ情報・`NDEBUG` は、dowel では構成の語彙が決める
/// （`default_compile_flags`）。取り込み（`migrate import`）はこれらを
/// 下書きへ写さず、等価判定（`migrate verify`）は両側から等しく除く。
/// 写すと下書きのフラグが無条件になり、release から取り込んだ下書きの
/// debug ビルドが最適化された `NDEBUG` 付きになる（issue #54）。
pub fn is_config_flag(word: &str) -> bool {
    if word == "-DNDEBUG" {
        return true;
    }
    if let Some(rest) = word.strip_prefix("-O") {
        return matches!(rest, "" | "0" | "1" | "2" | "3" | "s" | "z" | "g" | "fast");
    }
    if let Some(rest) = word.strip_prefix("-g") {
        // `-g` `-g3` `-ggdb3` `-gdwarf-5` 等。`-gcc-toolchain` のような
        // デバッグ情報と無関係な語は写す側に残す。
        return rest.is_empty()
            || rest.chars().all(|c| c.is_ascii_digit())
            || rest.starts_with("gdb")
            || rest.starts_with("dwarf")
            || rest == "line-tables-only"
            || rest == "split-dwarf";
    }
    false
}

/// 引数列を意味に効く部分へ落とす。
fn normalize(args: &[String], dir: &Path, source: &Path) -> Normalized {
    let mut out = Normalized::default();
    let mut i = 1; // 先頭はコンパイラ
                   // `-DX` と `-D X` の双方を受ける。前者は付随、後者は次の引数。
    let attached_or_next = |attached: &str, i: &mut usize| -> String {
        if attached.is_empty() {
            *i += 1;
            args.get(*i - 1).cloned().unwrap_or_default()
        } else {
            attached.to_string()
        }
    };
    while i < args.len() {
        let a = &args[i];
        i += 1;
        match a.as_str() {
            "-c" | "-MD" | "-MMD" | "-MP" => {}
            "-o" | "-MF" | "-MT" | "-MQ" => i += 1,
            _ if *a == source.display().to_string() => {}
            _ if a.starts_with("-D") => {
                let d = attached_or_next(&a[2..], &mut i);
                // `-DX` は `X=1` の意味（C プリプロセッサの規定）。
                let d = if d.contains('=') { d } else { format!("{d}=1") };
                // `NDEBUG` は構成が供給する（is_config_flag と同じ扱い）。
                if d == "NDEBUG=1" {
                    continue;
                }
                out.defines.insert(d);
            }
            _ if a.starts_with("-I") => {
                let inc = attached_or_next(&a[2..], &mut i);
                out.includes.insert(absolute(dir, Path::new(&inc)));
            }
            "-isystem" => {
                let inc = attached_or_next("", &mut i);
                out.includes.insert(absolute(dir, Path::new(&inc)));
            }
            _ => {
                // 構成のフラグは両側から等しく除く。
                if is_config_flag(a) {
                    continue;
                }
                // ソースは相対で書かれることもある。絶対化して一致すれば除く。
                if absolute(dir, Path::new(a)) == source {
                    continue;
                }
                *out.flags.entry(a.clone()).or_insert(0) += 1;
            }
        }
    }
    out
}

/// `directory` 基準の絶対パス。存在に依存しない字句的な正規化に留める
/// （参照側のビルド木は手元に無いことがある）。
fn absolute(dir: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `command` 文字列の分割。シェルの引用（`"` / `'` / `\`）だけを解釈する。
fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let mut in_word = false;
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' if in_word => {
                out.push(std::mem::take(&mut cur));
                in_word = false;
            }
            ' ' | '\t' => {}
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = chars.next() {
                                cur.push(e);
                            }
                        }
                        _ => cur.push(c),
                    }
                }
            }
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '\\' => {
                in_word = true;
                if let Some(e) = chars.next() {
                    cur.push(e);
                }
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// 人間向けの報告。
pub fn render_text(v: &Verdict) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} equivalent, {} differing, {} not ported, {} only in dowel\n",
        v.equivalent,
        v.differing.len(),
        v.unported.len(),
        v.extra_sources.len()
    ));
    for d in &v.differing {
        out.push_str(&format!("\n{}\n", d.file.display()));
        for m in &d.missing {
            out.push_str(&format!("  - {m}   (in the reference, not in dowel)\n"));
        }
        for e in &d.extra {
            out.push_str(&format!("  + {e}   (in dowel, not in the reference)\n"));
        }
    }
    if !v.unported.is_empty() {
        out.push_str("\nnot ported yet:\n");
        for f in &v.unported {
            out.push_str(&format!("  {}\n", f.display()));
        }
    }
    if !v.extra_sources.is_empty() {
        out.push_str("\nonly in dowel (tests and new targets are expected here):\n");
        for f in &v.extra_sources {
            out.push_str(&format!("  {}\n", f.display()));
        }
    }
    out
}

/// 機械可読の報告。
pub fn render_json(v: &Verdict) -> String {
    let mut w = dowel_support::json::JsonWriter::pretty();
    w.begin_object();
    w.field_u64("equivalent", v.equivalent as u64);
    w.key("differing").begin_array();
    for d in &v.differing {
        w.begin_object();
        w.field_str("file", &d.file.display().to_string());
        w.field_strs("missing", d.missing.iter().map(String::as_str));
        w.field_strs("extra", d.extra.iter().map(String::as_str));
        w.end_object();
    }
    w.end_array();
    w.field_strs("unported", v.unported.iter().map(|p| p.to_str().unwrap_or("")));
    w.field_strs("only_in_dowel", v.extra_sources.iter().map(|p| p.to_str().unwrap_or("")));
    w.end_object();
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(args: &[&str]) -> Normalized {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        normalize(&owned, Path::new("/b"), Path::new("/s/main.c"))
    }

    #[test]
    fn equivalent_commands_normalize_to_the_same_form() {
        // コンパイラ名、-o、depfile、-D の付け方、-I の相対・絶対は意味に効かない。
        let a = norm(&[
            "cc",
            "-DFOO",
            "-I",
            "include",
            "-c",
            "/s/main.c",
            "-o",
            "x.o",
            "-MD",
            "-MF",
            "x.d",
        ]);
        let b = norm(&["clang", "-D", "FOO=1", "-I/b/include", "-c", "/s/main.c", "-o", "y.o"]);
        assert!(a == b);
    }

    #[test]
    fn differing_defines_and_flags_are_detected() {
        let theirs = norm(&["cc", "-DFOO=2", "-Wall", "-c", "/s/main.c"]);
        let ours = norm(&["cc", "-DFOO=1", "-c", "/s/main.c"]);
        let d = diff(Path::new("/s/main.c"), &theirs, &ours);
        assert!(d.missing.contains(&"-DFOO=2".to_string()), "{:?}", d.missing);
        assert!(d.missing.contains(&"-Wall".to_string()));
        assert!(d.extra.contains(&"-DFOO=1".to_string()));
    }

    #[test]
    fn command_strings_split_like_a_shell() {
        assert_eq!(
            split_command(r#"cc -DNAME="a b" -I'/x y' src/main.c"#),
            vec!["cc", "-DNAME=a b", "-I/x y", "src/main.c"]
        );
    }

    #[test]
    fn reads_both_reference_forms() {
        let text = r#"[
            {"directory": "/b", "file": "a.c", "arguments": ["cc", "-DA", "-c", "a.c"]},
            {"directory": "/b", "file": "/s/b.c", "command": "cc -DB -c /s/b.c"}
        ]"#;
        let entries = read_reference(text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].file, Path::new("/b/a.c"));
        assert!(entries[1].norm.defines.contains("B=1"));
    }
}
