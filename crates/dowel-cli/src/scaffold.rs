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
pub fn add_package(project: &Path, rel: &str) -> Result<(), String> {
    let manifest_path = project.join("dowel.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!("cannot read {}: {e}. run `dowel add` inside a package", manifest_path.display())
    })?;

    let dir = project.join(rel);
    let name = package_name(&dir)?;
    // 名前の重複は読み込みが `dep(\"名前\")` を一意に解決できなくする。
    if text.contains(&format!("name = \"{name}\"")) || text.contains(&format!("name=\"{name}\"")) {
        return Err(format!("`{name}` is already declared in {}", manifest_path.display()));
    }
    if dir.join("dowel.toml").exists() {
        return Err(format!("`{}` is already a dowel package", dir.display()));
    }

    write(&dir, "dowel.toml", &manifest(&name))?;
    write_lib_skeleton(&dir, &name)?;

    // 末尾に追記する。厳密な TOML では配列テーブルの位置に意味が無いため、
    // 既存の本文を触らずに済む。
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n[[dependencies]]\nname = \"{name}\"\npath = \"{rel}\"\n"));
    std::fs::write(&manifest_path, out).map_err(fs_err)?;

    eprintln!("created lib package `{name}` at {}", dir.display());
    eprintln!("declared it in {}", manifest_path.display());
    eprintln!("next: add `deps = [dep(\"{name}\")]` to a target block in dowel.build");
    Ok(())
}

/// パスの最終要素をパッケージ名にする。テーブル見出しの識別子として
/// 妥当であること（字句規則と同じ: 先頭は英字か `_`、以後は英数と `_` `-`）。
fn package_name(dir: &Path) -> Result<String, String> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("cannot take a package name from `{}`", dir.display()))?;
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
