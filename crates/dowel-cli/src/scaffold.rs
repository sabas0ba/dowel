//! 雛型の生成（`dowel new` / `dowel add`、issue #47）。
//!
//! 生成するのは動く最小構成である。飾りを増やすほど、生成直後に消す作業を
//! 利用者へ移すだけになる。生成物の形は `examples/hello` と揃え、
//! e2e が「生成 → ビルド → 実行」を通すことで雛型の陳腐化を防ぐ。
//!
//! `dowel add` の `dowel.toml` への追記は末尾への付け足しで行う。
//! `dowel.toml` は厳密な TOML であり（[ADR-0003]）、`[[dependencies]]` は
//! 末尾に置いても意味が変わらないため、既存の本文へ触れる必要がない。
//!
//! [ADR-0003]: ../../../docs/adr/0003-manifest-split.md

use std::path::Path;

/// `dowel new <path>`。新しいディレクトリにパッケージを作る。
pub fn new_package(dir: &Path, lib: bool) -> Result<(), String> {
    let name = package_name(dir)?;
    if dir.join("dowel.toml").exists() {
        return Err(format!("`{}` is already a dowel package", dir.display()));
    }
    if dir.exists() && std::fs::read_dir(dir).map_err(fs_err)?.next().is_some() {
        return Err(format!(
            "`{}` exists and is not empty; `dowel new` only writes into a fresh directory",
            dir.display()
        ));
    }

    write(dir, "dowel.toml", &manifest(&name))?;
    write(dir, ".gitignore", "/.dowel/\n/compile_commands.json\n")?;
    if lib {
        write_lib_skeleton(dir, &name)?;
    } else {
        write(dir, "dowel.build", &format!("[bin.{name}]\nsources = glob(\"src/*.c\")\n"))?;
        write(
            dir,
            "src/main.c",
            &format!(
                "#include <stdio.h>\n\nint main(void) {{\n    printf(\"hello from {name}\\n\");\n    return 0;\n}}\n"
            ),
        )?;
    }
    eprintln!("created {} package `{name}` at {}", kind_name(lib), dir.display());
    eprintln!("next: cd {} && dowel build", dir.display());
    Ok(())
}

/// `dowel add <path>`。カレントのパッケージ配下に lib パッケージを作り、
/// `dowel.toml` へ `path` 依存を追記する。
pub fn add_package(project: &Path, rel: &str, name: Option<&str>) -> Result<(), String> {
    let dir = project.join(rel);
    let name = match name {
        Some(n) => valid_name(n)?,
        None => package_name(&dir)?,
    };
    let manifest_path = read_manifest_for_add(project, &name)?.0;
    if dir.join("dowel.toml").exists() {
        return Err(format!("`{}` is already a dowel package", dir.display()));
    }

    write(&dir, "dowel.toml", &manifest(&name))?;
    write_lib_skeleton(&dir, &name)?;
    append_dependency(project, &name, &format!("path = \"{rel}\""))?;

    eprintln!("created lib package `{name}` at {}", dir.display());
    eprintln!("declared it in {}", manifest_path.display());
    eprintln!("next: add `deps = [dep(\"{name}\")]` to a target block in dowel.build");
    Ok(())
}

/// `dowel add --git <url> [--rev <rev>]`。git 依存を `dowel.toml` へ宣言する。
///
/// マニフェストに書かれるのはフル 40 桁の sha のみ（docs/11-toml-reference.md）。
/// 名前や省略時の HEAD はここで一度だけ `git ls-remote` により解決する。
/// dowelup の pin と同じ判断で、解決を書き込み時に済ませ、
/// 読み込みは以後ネットワークに依存しない。
pub fn add_git_dependency(
    project: &Path,
    url: &str,
    rev: Option<&str>,
    name: Option<&str>,
) -> Result<(), String> {
    let name = match name {
        Some(n) => valid_name(n)?,
        None => name_from_url(url)?,
    };
    let (manifest_path, _) = read_manifest_for_add(project, &name)?;

    let rev = match rev {
        Some(r) if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) => {
            r.to_ascii_lowercase()
        }
        Some(r) => ls_remote(url, r)?,
        None => ls_remote(url, "HEAD")?,
    };

    append_dependency(project, &name, &format!("git  = \"{url}\"\nrev  = \"{rev}\""))?;
    eprintln!("declared git dependency `{name}` at rev {rev} in {}", manifest_path.display());
    eprintln!("next: add `deps = [dep(\"{name}\")]` to a target block in dowel.build");
    eprintln!("      `dowel check` fetches it on first use");
    Ok(())
}

/// `dowel.toml` を読み、名前の重複を拒む。重複は読み込みが `dep("名前")` を
/// 一意に解決できなくする。
fn read_manifest_for_add(
    project: &Path,
    name: &str,
) -> Result<(std::path::PathBuf, String), String> {
    let manifest_path = project.join("dowel.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!("cannot read {}: {e}. run `dowel add` inside a package", manifest_path.display())
    })?;
    if text.contains(&format!("name = \"{name}\"")) || text.contains(&format!("name=\"{name}\"")) {
        return Err(format!("`{name}` is already declared in {}", manifest_path.display()));
    }
    Ok((manifest_path, text))
}

/// 末尾に追記する。厳密な TOML では配列テーブルの位置に意味が無いため、
/// 既存の本文を触らずに済む。
fn append_dependency(project: &Path, name: &str, source: &str) -> Result<(), String> {
    let manifest_path = project.join("dowel.toml");
    let mut out = std::fs::read_to_string(&manifest_path).map_err(fs_err)?;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n[[dependencies]]\nname = \"{name}\"\n{source}\n"));
    std::fs::write(&manifest_path, out).map_err(fs_err)
}

/// URL の最終要素（`.git` を除く）を依存名にする。
fn name_from_url(url: &str) -> Result<String, String> {
    let tail = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("");
    let tail = tail.strip_suffix(".git").unwrap_or(tail);
    valid_name(tail).map_err(|e| format!("{e}. pass `--name <name>` to choose one"))
}

/// 名前を一度だけ sha に解決する。書き込むのは解決済みの sha のみであり、
/// 名前だけの参照はマニフェストに残さない。
fn ls_remote(url: &str, what: &str) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(["ls-remote", url, what])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-remote failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match text.split_whitespace().next() {
        Some(sha) if sha.len() == 40 => Ok(sha.to_string()),
        _ => Err(format!("`{what}` does not resolve to a commit at `{url}`")),
    }
}

/// パスの最終要素をパッケージ名にする。
fn package_name(dir: &Path) -> Result<String, String> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("cannot take a package name from `{}`", dir.display()))?;
    valid_name(name)
}

/// テーブル見出しの識別子として妥当な名前であること
/// （字句規則と同じ: 先頭は英字か `_`、以後は英数と `_` `-`）。
fn valid_name(name: &str) -> Result<String, String> {
    let mut chars = name.bytes();
    let head_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    if head_ok && rest_ok {
        Ok(name.to_string())
    } else {
        Err(format!(
            "`{name}` cannot be a package name: it must start with a letter or `_` and \
             contain only letters, digits, `_`, and `-`"
        ))
    }
}

fn kind_name(lib: bool) -> &'static str {
    if lib {
        "lib"
    } else {
        "bin"
    }
}

fn manifest(name: &str) -> String {
    format!("[package]\nname    = \"{name}\"\nversion = \"0.1.0\"\n")
}

fn write_lib_skeleton(dir: &Path, name: &str) -> Result<(), String> {
    // C の識別子に `-` は使えない。関数名の側だけ写像する。
    let c = name.replace('-', "_");
    write(
        dir,
        "dowel.build",
        &format!(
            r#"[lib.{name}]
sources = glob("src/*.c")

[lib.{name}.public]
includes = [dir("include")]

[lib.{name}.private]
includes = [dir("src")]

[test.{name}_test]
sources = glob("tests/*.c")

[test.{name}_test.private]
deps = [target("{name}")]
"#
        ),
    )?;
    write(dir, &format!("include/{name}.h"), &format!("#pragma once\n\nint {c}_answer(void);\n"))?;
    write(
        dir,
        &format!("src/{name}.c"),
        &format!("#include \"{name}.h\"\n\nint {c}_answer(void) {{\n    return 42;\n}}\n"),
    )?;
    write(
        dir,
        &format!("tests/{name}_test.c"),
        &format!(
            "#include \"{name}.h\"\n\nint main(void) {{\n    return {c}_answer() == 42 ? 0 : 1;\n}}\n"
        ),
    )
}

fn write(dir: &Path, rel: &str, contents: &str) -> Result<(), String> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(fs_err)?;
    }
    std::fs::write(&path, contents).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn fs_err(e: std::io::Error) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_follow_the_identifier_rule() {
        assert!(package_name(Path::new("x/my-lib_2")).is_ok());
        assert!(package_name(Path::new("_private")).is_ok());
        assert!(package_name(Path::new("2fast")).is_err(), "a leading digit is not an ident");
        assert!(package_name(Path::new("na me")).is_err());
        assert!(package_name(Path::new("日本語")).is_err());
    }
}
