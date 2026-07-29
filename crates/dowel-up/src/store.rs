//! `$DOWELUP_HOME` の配置と、pin / 既定による版の選択。
//!
//! 配置（docs/61-acquisition.md）:
//!
//! - `versions/<sha>/bin/dowel` — インストール済みの実体
//! - `versions/<sha>/origin` — どの指定子・どの上流から解決したかの記録
//! - `upstream.git` — 解決と取得に使う mirror
//! - `default` — pin が無い場所で使う sha
//! - `tmp/<sha>` — ビルド中の作業木
//!
//! pin（`.dowel-version`）と `default` に書くのは解決済みの sha だけである。
//! タグやブランチの名前は可動であり、固定とみなさない
//! （docs/adr/0013-self-acquisition.md）。

use crate::spec;
use std::path::{Path, PathBuf};

pub const PIN_FILE: &str = ".dowel-version";

pub struct Home {
    pub root: PathBuf,
}

impl Home {
    /// `DOWELUP_HOME` → `$HOME/.dowel` の順で決める。
    pub fn locate() -> Result<Home, String> {
        if let Some(v) = std::env::var_os("DOWELUP_HOME") {
            if !v.is_empty() {
                return Ok(Home { root: PathBuf::from(v) });
            }
        }
        match std::env::var_os("HOME") {
            Some(h) if !h.is_empty() => Ok(Home { root: PathBuf::from(h).join(".dowel") }),
            _ => {
                Err("cannot locate the dowelup home: neither DOWELUP_HOME nor HOME is set"
                    .to_string())
            }
        }
    }

    pub fn mirror(&self) -> PathBuf {
        self.root.join("upstream.git")
    }

    pub fn versions(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn version_dir(&self, sha: &str) -> PathBuf {
        self.versions().join(sha)
    }

    pub fn bin(&self, sha: &str) -> PathBuf {
        self.version_dir(sha).join("bin").join("dowel")
    }

    pub fn origin(&self, sha: &str) -> PathBuf {
        self.version_dir(sha).join("origin")
    }

    pub fn default_file(&self) -> PathBuf {
        self.root.join("default")
    }

    pub fn workdir(&self, sha: &str) -> PathBuf {
        self.root.join("tmp").join(sha)
    }
}

#[derive(Debug)]
pub struct Installed {
    pub sha: String,
    /// この sha を解決した指定子。入れた順。同じコミットを指す指定子は
    /// 複数ありうる（`stable` とそれが指すタグ）。
    pub specs: Vec<String>,
}

/// インストール済みの一覧。`origin` と実体の双方が揃うものだけを返す。
/// 中断の残骸を一覧に出さないためであり、揃わないものは無いものとして扱う。
pub fn installed(home: &Home) -> Vec<Installed> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(home.versions()) else { return out };
    for e in entries.flatten() {
        let sha = e.file_name().to_string_lossy().into_owned();
        if !home.bin(&sha).is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(home.origin(&sha)) else { continue };
        out.push(Installed { sha, specs: fields(&text, "spec") });
    }
    out.sort_by(|a, b| a.sha.cmp(&b.sha));
    out
}

/// 解決の記録を書く。既に記録がある場合は新しい指定子・上流を追記する。
///
/// 同じコミットを別の指定子で入れ直すのは通常の操作である（`stable` は
/// 最新の release タグそのものを指す）。最初の1つしか残さないと、
/// install が成功した指定子で `dowel +<指定子>` が選べない（issue #39）。
pub fn record_origin(home: &Home, sha: &str, spec: &str, url: &str) -> Result<(), String> {
    let file = home.origin(sha);
    let (mut specs, mut urls) = match std::fs::read_to_string(&file) {
        Ok(text) => (fields(&text, "spec"), fields(&text, "url")),
        Err(_) => (Vec::new(), Vec::new()),
    };
    if !specs.iter().any(|s| s == spec) {
        specs.push(spec.to_string());
    }
    if !urls.iter().any(|u| u == url) {
        urls.push(url.to_string());
    }
    let mut text = format!("sha={sha}\n");
    for s in &specs {
        text.push_str(&format!("spec={s}\n"));
    }
    for u in &urls {
        text.push_str(&format!("url={u}\n"));
    }
    std::fs::write(&file, text).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

/// `key=値` の行を全て集める。記録は追記されるため、値は1つとは限らない。
fn fields(text: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}=");
    text.lines().filter_map(|l| l.strip_prefix(&prefix).map(str::to_string)).collect()
}

/// インストール済みの中から1つ選ぶ。sha の接頭辞か、インストール時の
/// 指定子のいずれか（`nightly` や `branch:feature`）で照合する。
/// 後者は「そのときの解決結果」への別名であり、上流へは問い合わせない。
pub fn match_installed<'a>(list: &'a [Installed], needle: &str) -> Result<&'a Installed, String> {
    let lower = needle.to_ascii_lowercase();
    let by_sha = spec::is_hex(&lower);
    let hits: Vec<&Installed> = list
        .iter()
        .filter(|i| i.specs.iter().any(|s| s == needle) || (by_sha && i.sha.starts_with(&lower)))
        .collect();
    match hits.as_slice() {
        [] => Err(format!(
            "no installed version matches `{needle}`; `dowelup list` shows what is installed"
        )),
        [one] => Ok(one),
        many => Err(format!(
            "`{needle}` matches more than one installed version:\n{}",
            many.iter()
                .map(|i| format!("  {}  (from {})", i.sha, i.specs.join(", ")))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

pub enum Selection {
    Pin { file: PathBuf, sha: String },
    Default { sha: String },
}

/// `start` から上へ辿り、最初の pin ファイルを返す。
pub fn find_pin(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let p = d.join(PIN_FILE);
        if p.is_file() {
            return Some(p);
        }
        dir = d.parent();
    }
    None
}

pub fn select(home: &Home, start: &Path) -> Result<Selection, String> {
    if let Some(file) = find_pin(start) {
        let sha = read_selection(&file)?;
        return Ok(Selection::Pin { file, sha });
    }
    let def = home.default_file();
    if def.is_file() {
        return Ok(Selection::Default { sha: read_selection(&def)? });
    }
    Err("no version is selected: no .dowel-version file up the tree, and no default; \
         run `dowelup pin <spec>` or `dowelup default <spec>`"
        .to_string())
}

/// pin / default ファイルから sha を読む。
///
/// sha 以外（チャネル名やブランチ名）は解決せずに拒む。shim が暗黙に
/// ネットワークへ触れないための制約である（docs/adr/0013-self-acquisition.md）。
pub fn read_selection(file: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.len() == 40 && spec::is_hex(line) {
            return Ok(line.to_ascii_lowercase());
        }
        return Err(format!(
            "{} contains `{line}`, which is not a full commit hash; \
             run `dowelup pin {line}` to resolve it and rewrite the file",
            file.display()
        ));
    }
    Err(format!("{} does not contain a commit hash", file.display()))
}

pub fn write_selection(file: &Path, sha: &str, spec: &str) -> Result<(), String> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = format!("# Managed by dowelup. Resolved from \"{spec}\".\n{sha}\n");
    std::fs::write(file, text).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the crate lives two levels below the repository root")
            .join("target")
            .join("dowelup-unit")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("cannot create the scratch directory");
        root
    }

    #[test]
    fn the_pin_search_prefers_the_nearest_file() {
        let root = scratch("pin-walk");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(PIN_FILE), "x").unwrap();
        assert_eq!(find_pin(&nested), Some(root.join(PIN_FILE)));
        std::fs::write(nested.join(PIN_FILE), "y").unwrap();
        assert_eq!(find_pin(&nested), Some(nested.join(PIN_FILE)));
    }

    #[test]
    fn a_selection_that_is_not_a_hash_tells_how_to_fix_it() {
        let root = scratch("pin-name");
        let file = root.join(PIN_FILE);
        let sha = "0123456789abcdef0123456789abcdef01234567";
        write_selection(&file, sha, "nightly").unwrap();
        assert_eq!(read_selection(&file).unwrap(), sha);
        // 手書きの名前は解決しない。誘導だけを返す。
        std::fs::write(&file, "nightly\n").unwrap();
        let e = read_selection(&file).unwrap_err();
        assert!(e.contains("dowelup pin nightly"), "{e}");
    }

    #[test]
    fn an_ambiguous_needle_is_rejected_with_the_candidates() {
        let list = vec![
            Installed {
                sha: "aaa1111111111111111111111111111111111111".to_string(),
                specs: vec!["nightly".to_string()],
            },
            Installed {
                sha: "aab2222222222222222222222222222222222222".to_string(),
                specs: vec!["branch:feature".to_string()],
            },
        ];
        assert_eq!(match_installed(&list, "aaa").unwrap().specs, ["nightly"]);
        assert_eq!(match_installed(&list, "branch:feature").unwrap().specs, ["branch:feature"]);
        let e = match_installed(&list, "aa").unwrap_err();
        assert!(e.contains("aaa1") && e.contains("aab2"), "{e}");
        let e = match_installed(&list, "zzz").unwrap_err();
        assert!(e.contains("dowelup list"), "{e}");
    }

    #[test]
    fn recording_the_same_sha_again_appends_the_new_specifier() {
        // 同じコミットを指す指定子は複数ありうる（issue #39）。
        // どの指定子で入れても、その指定子で照合できること。
        let root = scratch("origin-append");
        let home = Home { root };
        let sha = "aaa1111111111111111111111111111111111111";
        std::fs::create_dir_all(home.version_dir(sha)).unwrap();

        record_origin(&home, sha, "stable", "https://example.invalid/up").unwrap();
        record_origin(&home, sha, "tag:v0.9.0", "https://example.invalid/up").unwrap();
        // 同じ指定子の入れ直しは重複させない。
        record_origin(&home, sha, "stable", "https://example.invalid/up").unwrap();

        let text = std::fs::read_to_string(home.origin(sha)).unwrap();
        assert_eq!(fields(&text, "spec"), ["stable", "tag:v0.9.0"]);
        assert_eq!(fields(&text, "url"), ["https://example.invalid/up"]);

        let list = vec![Installed { sha: sha.to_string(), specs: fields(&text, "spec") }];
        assert!(match_installed(&list, "stable").is_ok());
        assert!(match_installed(&list, "tag:v0.9.0").is_ok());
    }
}
