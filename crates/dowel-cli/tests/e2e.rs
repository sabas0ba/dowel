//! e2e。実際に C をコンパイルし、リンクし、実行する。
//!
//! 単体テストが「アクショングラフが期待通りか」を見るのに対し、ここは
//! 「そのグラフを実行すると本当に動く実行ファイルができるか」を見る。
//! 2つの間には、フラグの引用、インクルード探索の順序、リンク順、
//! 再ビルドの判定といった、机上では落ちない差がある。

mod common;

use common::{build_dir, run_artifact, Project};

/// libfoo（静的ライブラリ）と app（実行ファイル）の2パッケージ。
///
/// 公開ヘッダは `libfoo/include`、非公開ヘッダは `libfoo/src` に置く。
/// app から前者は見えて後者は見えないことが、`public` / `private` の実効的な検査になる。
fn two_package_project(name: &str) -> Project {
    let p = Project::new(name);
    p.write("libfoo/dowel.toml", "[package]\nname    = \"libfoo\"\nversion = \"0.1.0\"\n");
    p.write(
        "libfoo/dowel.build",
        r#"
[lib.foo]
sources = glob("src/**.c")

[lib.foo.public]
includes = [dir("include")]
defines  = { FOO_API = 1 }

[lib.foo.private]
includes = [dir("src")]
flags    = ["-Wall", "-Wextra", "-Werror"]
"#,
    );
    p.write("libfoo/include/foo.h", "#pragma once\nint foo_add(int a, int b);\n");
    p.write(
        "libfoo/src/internal.h",
        "#pragma once\n#define FOO_BIAS 0\nstatic inline int bias(void) { return FOO_BIAS; }\n",
    );
    p.write(
        "libfoo/src/foo.c",
        "#include \"foo.h\"\n#include \"internal.h\"\nint foo_add(int a, int b) { return a + b + bias(); }\n",
    );

    p.write(
        "app/dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"libfoo\"\npath = \"../libfoo\"\n",
    );
    p.write(
        "app/dowel.build",
        r#"
[bin.app]
sources = glob("src/*.c")

[bin.app.private]
deps  = [dep("libfoo")]
flags = match cfg.opt {
    debug   => ["-DAPP_OPT=0"],
    release => ["-DAPP_OPT=1"],
}
"#,
    );
    p.write(
        "app/src/main.c",
        r#"#include <stdio.h>
#include "foo.h"
int main(void) {
    printf("sum=%d opt=%d api=%d\n", foo_add(2, 3), APP_OPT, FOO_API);
    return 0;
}
"#,
    );
    p
}

#[test]
fn 二つのパッケージをビルドして実行できる() {
    let p = two_package_project("build-run");
    p.run("app", &["check"]).success();
    p.run("app", &["build"]).success().stderr_contains("できた:");

    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert!(bin.exists(), "実行ファイルがない: {}", bin.display());
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");
}

#[test]
fn 直接実行器でも同じ成果物ができる() {
    let p = two_package_project("direct");
    p.run("app", &["build", "--executor=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");
}

#[test]
fn 構成を変えるとフラグが変わる() {
    let p = two_package_project("configs");
    p.run("app", &["build", "--config=release"]).success();
    let bin = build_dir(&p.path("app"), "release").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=1 api=1\n");

    // 構成ごとにビルドディレクトリが分かれ、互いを壊さない。
    p.run("app", &["build", "--config=debug"]).success();
    assert_eq!(run_artifact(&bin), "sum=5 opt=1 api=1\n", "release の成果物が上書きされた");
    let debug_bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&debug_bin), "sum=5 opt=0 api=1\n");
}

#[test]
fn 非公開のインクルードは依存元へ漏れない() {
    let p = two_package_project("private-include");
    // app から libfoo の非公開ヘッダを読もうとすると、コンパイルが失敗するはず。
    p.write("app/src/main.c", "#include \"internal.h\"\nint main(void) { return bias(); }\n");
    let r = p.run("app", &["build", "--executor=direct"]);
    r.failure();
    assert!(r.stderr.contains("internal.h"), "コンパイラの診断が見えない\n{r}");
}

#[test]
fn コンパイル失敗は非零で終わり原因を見せる() {
    let p = two_package_project("compile-error");
    // 未宣言の識別子を値として使う。関数呼び出しだと暗黙宣言が効いて
    // リンク時まで落ちないため、コンパイル時に確実に失敗する形にする。
    p.write("app/src/main.c", "int main(void) { return undefined_symbol_xyz; }\n");
    let r = p.run("app", &["build", "--executor=direct"]);
    r.failure();
    r.stderr_contains("undefined_symbol_xyz");
    // どのアクションが失敗したかが分かること。
    r.stderr_contains("CC ");
}

#[test]
fn 再ビルドでは何も実行しない() {
    let p = two_package_project("incremental");
    p.run("app", &["build", "--executor=direct"]).success();
    let second = p.run("app", &["build", "--executor=direct", "--log-level=debug"]);
    second.success().stderr_contains("実行 0 件");
}

#[test]
fn ヘッダを変えると再コンパイルされる() {
    let p = two_package_project("depfile");
    p.run("app", &["build", "--executor=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");

    // ソースではなくヘッダだけを変える。depfile を読めていなければ再実行されない。
    p.write(
        "libfoo/src/internal.h",
        "#pragma once\n#define FOO_BIAS 100\nstatic inline int bias(void) { return FOO_BIAS; }\n",
    );
    let r = p.run("app", &["build", "--executor=direct", "--log-level=debug"]);
    r.success();
    assert!(!r.stderr.contains("実行 0 件"), "ヘッダ変更が伝わっていない\n{r}");
    assert_eq!(run_artifact(&bin), "sum=105 opt=0 api=1\n");
}

#[test]
fn compile_commands_を書き出す() {
    let p = two_package_project("compdb");
    p.run("app", &["build"]).success();

    for path in [
        p.path("app/compile_commands.json"),
        build_dir(&p.path("app"), "debug").join("compile_commands.json"),
    ] {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} を読めない: {e}", path.display()));
        assert!(text.contains("\"arguments\""), "{}", path.display());
        assert!(text.contains("main.c"), "{}", path.display());
        assert!(text.contains("foo.c"), "{}", path.display());
        // 伝播したインクルードがコンパイル引数に入っていること。
        assert!(text.contains("libfoo/include"), "{}", path.display());
    }
}

#[test]
fn 機能フラグで依存とフラグが切り替わる() {
    let p = Project::new("features");
    p.write("libz/dowel.toml", "[package]\nname = \"libz\"\nversion = \"0\"\n");
    p.write(
        "libz/dowel.build",
        "[lib.z]\nsources = glob(\"*.c\")\n\n[lib.z.public]\nincludes = [dir(\".\")]\ndefines  = { HAVE_Z = 1 }\n",
    );
    p.write("libz/z.h", "#pragma once\nint z_value(void);\n");
    p.write("libz/z.c", "#include \"z.h\"\nint z_value(void) { return 7; }\n");

    p.write(
        "dowel.toml",
        r#"
[package]
name    = "app"
version = "0.1.0"

[[dependencies]]
name     = "libz"
path     = "libz"
optional = true

[features]
zlib = ["libz"]
"#,
    );
    p.write(
        "dowel.build",
        r#"
[bin.app]
sources = glob("src/*.c")

[bin.app.private]
deps = [dep("libz") when feature.zlib]
"#,
    );
    p.write(
        "src/main.c",
        r#"#include <stdio.h>
#ifdef HAVE_Z
#include "z.h"
#endif
int main(void) {
#ifdef HAVE_Z
    printf("z=%d\n", z_value());
#else
    printf("no-z\n");
#endif
    return 0;
}
"#,
    );

    p.run(".", &["build"]).success();
    let bin = build_dir(&p.root, "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "no-z\n");

    p.run(".", &["build", "--features=zlib"]).success();
    let bin = build_dir(&p.root, "zlib").join("bin/app");
    assert_eq!(run_artifact(&bin), "z=7\n");
}

#[test]
fn 診断は位置と修正提案を持つ() {
    let p = Project::new("diagnostics");
    p.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.a]\nsources = glob(\"*.c\")\n\n[lib.a.public]\ninclude = [dir(\"x\")]\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("error[unknown-property]");
    r.stderr_contains("dowel.build:5:1");
    r.stderr_contains("includes");

    // JSON 形式は stdout に1行1診断で出る。
    let j = p.run(".", &["check", "--message-format=json"]);
    j.failure();
    j.stdout_contains("\"code\":\"unknown-property\"");
    j.stdout_contains("\"replacement\":\"includes\"");
    assert_eq!(j.stdout.lines().count(), 1, "1診断1行でない\n{j}");
}

#[test]
fn 構文誤りがあっても後続の診断が出る() {
    let p = Project::new("syntax-recovery");
    p.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    p.write("dowel.build", "[lib.a]\nsources = @@@\n\n[lib.a.public]\ninclude = [dir(\"x\")]\n");
    let r = p.run(".", &["check"]);
    r.failure();
    // 構文誤りで止まらず、意味解析の診断も同じ実行で出る。
    r.stderr_contains("unknown-char");
    r.stderr_contains("unknown-property");
}

#[test]
fn why_は伝播経路を辿れる() {
    let p = two_package_project("why");
    let r = p.run("app", &["why", "app:app", "includes"]);
    r.success();
    r.stdout_contains("dir(...)");
    r.stdout_contains("includes of libfoo:foo");
    r.stdout_contains("libfoo/dowel.build:");

    let j = p.run("app", &["why", "app:app", "includes", "--format=json"]);
    j.success();
    j.stdout_contains("\"provenance\"");
    j.stdout_contains("\"merge\": \"union\"");
}

#[test]
fn グラフを三形式で書き出せる() {
    let p = two_package_project("graph");
    p.run("app", &["graph"]).success().stdout_contains("app:app [bin]");
    p.run("app", &["graph", "--format=dot"]).success().stdout_contains("digraph dowel");
    let j = p.run("app", &["graph", "--format=json"]);
    j.success().stdout_contains("\"label\": \"app:app\"");

    // アクショングラフも同じ入口から見える。
    let a = p.run("app", &["graph", "--kind=action"]);
    a.success().stdout_contains("CC ");
    a.stdout_contains("AR ");
    a.stdout_contains("LINK ");
    let aj = p.run("app", &["graph", "--kind=action", "--format=json"]);
    aj.success().stdout_contains("\"kind\": \"cc\"");
}

#[test]
fn schema_dump_はマニフェストなしで出る() {
    let p = Project::new("schema");
    let r = p.run(".", &["schema", "dump"]);
    r.success();
    r.stdout_contains("\"merge\": \"error_on_conflict\"");
    r.stdout_contains("\"name\": \"includes\"");
    r.stdout_contains("\"name\": \"cfg.opt\"");
    // 語彙が暫定であることが出力自体から分かる。
    r.stdout_contains("Q1");
}

#[test]
fn abi_ラベルの不一致はビルド前に止める() {
    let p = Project::new("abi");
    p.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        r#"
[lib.a]
sources = glob("a.c")
[lib.a.public]
abi = "gnu11-cxx11abi1"

[lib.b]
sources = glob("b.c")
[lib.b.public]
abi = "gnu11-cxx11abi0"

[bin.app]
sources = glob("main.c")
[bin.app.private]
deps = [target("a"), target("b")]
"#,
    );
    p.write("a.c", "int a(void) { return 1; }\n");
    p.write("b.c", "int b(void) { return 2; }\n");
    p.write("main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["build"]);
    r.failure();
    r.stderr_contains("abi-mismatch");
    // 失敗はリンク前。実行ファイルは作られない。
    assert!(!p.path(".dowel/build").join("bin/app").exists());
}

#[test]
fn 取得できない依存は明示的に断る() {
    let p = Project::new("registry-dep");
    p.write(
        "dowel.toml",
        "[package]\nname = \"p\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"zlib\"\nversion = \"1.3\"\n",
    );
    p.write("dowel.build", "[bin.app]\nsources = glob(\"*.c\")\n");
    p.write("main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unsupported-dependency");
    r.stderr_contains("Phase 5");
}

#[test]
fn ログに依存グラフとアクション数が出る() {
    let p = two_package_project("logging");
    let r = p.run("app", &["build", "--log-level=trace"]);
    r.success();
    r.stderr_contains("依存グラフ:");
    r.stderr_contains("app:app → libfoo:foo");
    r.stderr_contains("アクション");
    // 段階ごとの所要時間が出る。
    r.stderr_contains("← plan");
    r.stderr_contains("← execute");

    // JSON 形式のログは1行1オブジェクト。
    let j = p.run("app", &["check", "--log-level=debug", "--log-format=json"]);
    j.success();
    let line = j.stderr.lines().find(|l| l.contains("\"level\"")).expect("JSON ログがない");
    assert!(line.starts_with('{') && line.ends_with('}'), "{line}");
}
