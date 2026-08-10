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
fn builds_and_runs_two_packages() {
    let p = two_package_project("build-run");
    p.run("app", &["check"]).success();
    p.run("app", &["build"]).success().stderr_contains("built:");

    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert!(bin.exists(), "the executable is missing: {}", bin.display());
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");
}

#[test]
fn the_direct_backend_produces_the_same_artifact() {
    let p = two_package_project("direct");
    p.run("app", &["build", "--backend=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");
}

#[test]
fn changing_the_configuration_changes_the_flags() {
    let p = two_package_project("configs");
    p.run("app", &["build", "--config=release"]).success();
    let bin = build_dir(&p.path("app"), "release").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=1 api=1\n");

    // 構成ごとにビルドディレクトリが分かれ、互いを壊さない。
    p.run("app", &["build", "--config=debug"]).success();
    assert_eq!(run_artifact(&bin), "sum=5 opt=1 api=1\n", "the release artifact was overwritten");
    let debug_bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&debug_bin), "sum=5 opt=0 api=1\n");
}

#[test]
fn private_includes_do_not_leak_to_dependents() {
    let p = two_package_project("private-include");
    // app から libfoo の非公開ヘッダを読もうとすると、コンパイルが失敗するはず。
    p.write("app/src/main.c", "#include \"internal.h\"\nint main(void) { return bias(); }\n");
    let r = p.run("app", &["build", "--backend=direct"]);
    r.failure();
    assert!(r.stderr.contains("internal.h"), "the compiler diagnostic is not visible\n{r}");
}

#[test]
fn a_compile_failure_exits_nonzero_and_shows_the_cause() {
    let p = two_package_project("compile-error");
    // 未宣言の識別子を値として使う。関数呼び出しだと暗黙宣言が効いて
    // リンク時まで落ちないため、コンパイル時に確実に失敗する形にする。
    p.write("app/src/main.c", "int main(void) { return undefined_symbol_xyz; }\n");
    let r = p.run("app", &["build", "--backend=direct"]);
    r.failure();
    r.stderr_contains("undefined_symbol_xyz");
    // どのアクションが失敗したかが分かること。
    r.stderr_contains("CC ");
}

#[test]
fn a_rebuild_runs_nothing() {
    let p = two_package_project("incremental");
    p.run("app", &["build", "--backend=direct"]).success();
    let second = p.run("app", &["build", "--backend=direct", "--log-level=trace"]);
    second.success().stderr_contains("ran 0 steps");
    // 何が最新と判定されたかが個別に見える。
    second.stderr_contains("up to date: CC ");
}

#[test]
fn touching_a_header_triggers_recompilation() {
    let p = two_package_project("depfile");
    p.run("app", &["build", "--backend=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");

    // ソースではなくヘッダだけを変える。depfile を読めていなければ再実行されない。
    p.write(
        "libfoo/src/internal.h",
        "#pragma once\n#define FOO_BIAS 100\nstatic inline int bias(void) { return FOO_BIAS; }\n",
    );
    let r = p.run("app", &["build", "--backend=direct", "--log-level=trace"]);
    r.success();
    assert!(!r.stderr.contains("ran 0 steps"), "the header change did not propagate\n{r}");
    // 再実行の理由が出ること。depfile 経由で拾ったヘッダが名指しされる。
    r.stderr_contains("stale: ");
    r.stderr_contains("internal.h");
    assert_eq!(run_artifact(&bin), "sum=105 opt=0 api=1\n");
}

#[test]
fn a_header_change_is_seen_after_building_with_the_other_backend() {
    // issue #41: ninja で組んだツリーを direct で組み直す。依存の記録が
    // 実行器の実装詳細に畳まれていると、ヘッダの変更が黙って見落とされ、
    // 古い成果物が残る。
    let p = two_package_project("cross-backend-header");
    p.run("app", &["build"]).success(); // 既定の ninja
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");

    p.write(
        "libfoo/src/internal.h",
        "#pragma once\n#define FOO_BIAS 100\nstatic inline int bias(void) { return FOO_BIAS; }\n",
    );
    let r = p.run("app", &["build", "--backend=direct", "--log-level=trace"]);
    r.success();
    assert!(!r.stderr.contains("ran 0 steps"), "the header change did not propagate\n{r}");
    assert_eq!(run_artifact(&bin), "sum=105 opt=0 api=1\n");
}

#[test]
fn the_artifact_is_up_to_date_after_crossing_backends() {
    // issue #41 の裏面。何も変えずに実行器を替えただけなら、全てを
    // 作り直すのではなく最新と判定される。依存の記録（depfile）が
    // 実行器を跨いで残っていることの検査である。
    let p = two_package_project("cross-backend-clean");
    p.run("app", &["build"]).success(); // 既定の ninja
    let r = p.run("app", &["build", "--backend=direct", "--log-level=debug"]);
    r.success().stderr_contains("ran 0 steps");
}

#[test]
fn the_make_backend_produces_the_same_artifact() {
    // ADR-0018: 出力段が ninja に固有の形をしていないことの検査。
    // 同じビルドグラフから別の生成器が同じ実行ファイルを作る。
    if !program_exists("make") {
        return;
    }
    let p = two_package_project("make-backend");
    p.run("app", &["build", "--backend=make"]).success();
    assert!(build_dir(&p.path("app"), "debug").join("Makefile").exists());
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");
}

#[test]
fn a_rebuild_with_make_leaves_the_artifact_alone() {
    // 生成した Makefile が依存を持てていること。持てていなければ毎回
    // 全てを組み直し、増分ビルドという前提が崩れる。
    if !program_exists("make") {
        return;
    }
    let p = two_package_project("make-incremental");
    p.run("app", &["build", "--backend=make"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    let first = std::fs::metadata(&bin).unwrap().modified().unwrap();
    p.run("app", &["build", "--backend=make"]).success();
    let second = std::fs::metadata(&bin).unwrap().modified().unwrap();
    assert_eq!(first, second, "make relinked an artifact that was already up to date");
}

#[test]
fn a_header_change_is_seen_by_make() {
    // ヘッダ依存は depfile 経由。make には `-include` で読ませている。
    if !program_exists("make") {
        return;
    }
    let p = two_package_project("make-depfile");
    p.run("app", &["build", "--backend=make"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");
    p.write(
        "libfoo/src/internal.h",
        "#pragma once\n#define FOO_BIAS 100\nstatic inline int bias(void) { return FOO_BIAS; }\n",
    );
    p.run("app", &["build", "--backend=make"]).success();
    assert_eq!(run_artifact(&bin), "sum=105 opt=0 api=1\n");
}

#[test]
fn the_graph_backend_writes_a_document_and_builds_nothing() {
    let p = two_package_project("graph-backend");
    let r = p.run("app", &["build", "--backend=graph"]);
    r.success();
    let doc = build_dir(&p.path("app"), "debug").join("build-graph.json");
    r.stderr_contains("wrote:");
    assert!(doc.exists(), "the document is missing: {}", doc.display());
    // 成果物が出来ていないのに「built:」と述べていないこと。
    assert!(!r.stderr.contains("built:"), "the graph backend claimed a build\n{r}");
    assert!(!build_dir(&p.path("app"), "debug").join("bin/app").exists());

    // 外の道具が読める形であること。名前と版が入っていて、読み直せる。
    let text = std::fs::read_to_string(&doc).unwrap();
    let g = dowel_build::backend::graph::parse(&text).expect("the document does not read back");
    assert!(g.steps.iter().any(|s| s.kind == dowel_build::ActionKind::Link));
    assert!(g.steps.iter().all(|s| s.outputs.iter().all(|o| o.is_absolute())));
    assert!(!g.default_outputs.is_empty());
}

#[test]
fn the_action_graph_and_the_emitted_document_are_the_same_thing() {
    // アクショングラフの JSON 表現が2つあると、読む側と走る側が黙ってずれる。
    let p = two_package_project("graph-one-shape");
    p.run("app", &["build", "--backend=graph"]).success();
    let written =
        std::fs::read_to_string(build_dir(&p.path("app"), "debug").join("build-graph.json"))
            .unwrap();
    let printed = p.run("app", &["graph", "--kind=action", "--format=json"]);
    printed.success();
    assert_eq!(printed.stdout, written);
}

#[test]
fn a_backend_that_does_not_build_is_refused_where_a_build_is_needed() {
    let p = two_package_project("graph-refused");
    let r = p.run("app", &["test", "--backend=graph"]);
    r.failure();
    r.stderr_contains("does not build");
}

#[test]
fn an_unknown_backend_names_the_ones_that_exist() {
    let p = two_package_project("backend-unknown");
    let r = p.run("app", &["build", "--backend=bazel"]);
    r.failure();
    r.stderr_contains("bazel");
    r.stderr_contains("ninja, direct, make, graph");
}

#[test]
fn the_old_executor_flag_names_its_replacement() {
    // 取る値の集合が変わったため、黙って受けない。
    let p = two_package_project("backend-renamed");
    let r = p.run("app", &["build", "--executor=direct"]);
    r.failure();
    r.stderr_contains("`--backend`");
}

#[test]
fn writes_compile_commands() {
    let p = two_package_project("compdb");
    p.run("app", &["build"]).success();

    for path in [
        p.path("app/compile_commands.json"),
        build_dir(&p.path("app"), "debug").join("compile_commands.json"),
    ] {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        assert!(text.contains("\"arguments\""), "{}", path.display());
        assert!(text.contains("main.c"), "{}", path.display());
        assert!(text.contains("foo.c"), "{}", path.display());
        // 伝播したインクルードがコンパイル引数に入っていること。
        assert!(text.contains("libfoo/include"), "{}", path.display());
    }
}

#[test]
fn the_build_leaves_no_stray_files_in_the_project() {
    let p = two_package_project("no-stray-files");
    p.run("app", &["build"]).success();

    // ninja の作業ファイルはビルドディレクトリ内に限定する。
    // 利用者のプロジェクトへ勝手に物を置かない。
    for stray in [".ninja_log", ".ninja_deps", "build.ninja"] {
        assert!(!p.path("app").join(stray).exists(), "`{stray}` was left in the project root");
        assert!(!p.path("libfoo").join(stray).exists(), "`{stray}` was left in libfoo");
    }
    // ビルドディレクトリの側にはある。
    let bd = build_dir(&p.path("app"), "debug");
    assert!(bd.join("build.ninja").exists());
    assert!(bd.join(".ninja_log").exists(), "ninja did not write its log into the build dir");

    // 意図して置くのは compile_commands.json だけ（clangd がここしか見ないため）。
    let entries: Vec<String> = std::fs::read_dir(p.path("app"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "src" && n != "dowel.toml" && n != "dowel.build" && n != ".dowel")
        .collect();
    assert_eq!(entries, vec!["compile_commands.json".to_string()], "unexpected files: {entries:?}");
}

#[test]
fn feature_flags_switch_dependencies_and_defines() {
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

/// 有効でない任意の依存を持つプロジェクト。実体は与えない。
fn project_with_an_absent_optional_dependency(name: &str) -> Project {
    let p = Project::new(name);
    p.write(
        "dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n\
         [features]\ndefault = []\nabsent  = []\n\n\
         [[dependencies]]\nname     = \"absent\"\npath     = \"../does-not-exist\"\noptional = true\n",
    );
    p.write(
        "dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.private]\ndeps = [dep(\"absent\") when feature.absent]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");
    p
}

#[test]
fn an_optional_dependency_that_is_off_is_not_read() {
    // 実体が無くても通る。読み込むと、選ばれていない依存に実体を要求することに
    // なり、取得を伴う供給形態では取得まで走ることになる。
    let p = project_with_an_absent_optional_dependency("optional-absent");
    p.run(".", &["check"]).success();

    // 有効にすれば読みに行く。実体が無いため、そこで初めて落ちる。
    p.run(".", &["check", "--features=absent"]).failure().stderr_contains("missing-manifest");
}

#[test]
fn an_optional_dependency_that_is_off_is_not_a_node_in_the_graph() {
    // 辺だけが消えて節点が残ると、`dowel graph` の読み手には
    // 「この構成に含まれる」と読める。
    let p = Project::new("optional-node");
    p.write("json/dowel.toml", "[package]\nname = \"json\"\nversion = \"0\"\n");
    p.write("json/dowel.build", "[lib.json]\nsources = glob(\"*.c\")\n");
    p.write("json/j.c", "int j(void) { return 1; }\n");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n\
         [features]\ndefault = [\"json\"]\njson    = []\n\n\
         [[dependencies]]\nname     = \"json\"\npath     = \"json\"\noptional = true\n",
    );
    p.write(
        "dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.private]\ndeps = [dep(\"json\") when feature.json]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let on = p.run(".", &["graph"]);
    on.success().stdout_contains("json:json");

    let off = p.run(".", &["graph", "--no-default-features"]);
    off.success();
    assert!(!off.stdout.contains("json"), "the disabled package is still a node\n{off}");
}

/// テスト対象のライブラリと、通るテスト／落ちるテストの2本。
///
/// テストハーネスは持たない。終了状態 0 が成功という C の慣習に従う。
fn project_with_tests(name: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"calc\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        r#"
[lib.calc]
sources = glob("src/*.c")

[lib.calc.public]
includes = [dir("src")]

[test.unit]
sources = glob("tests/unit.c")
[test.unit.private]
deps = [target("calc")]

[test.broken]
sources = glob("tests/broken.c")
[test.broken.private]
deps = [target("calc")]
"#,
    );
    p.write("src/calc.h", "#pragma once\nint add(int a, int b);\n");
    p.write("src/calc.c", "#include \"calc.h\"\nint add(int a, int b) { return a + b; }\n");
    p.write(
        "tests/unit.c",
        "#include <stdio.h>\n#include \"calc.h\"\nint main(void) { printf(\"sum=%d\\n\", add(2, 3)); return add(2, 3) == 5 ? 0 : 1; }\n",
    );
    p.write(
        "tests/broken.c",
        "#include <stdio.h>\n#include \"calc.h\"\nint main(void) { fprintf(stderr, \"expected 6, got %d\\n\", add(2, 3)); return 1; }\n",
    );
    p
}

#[test]
fn test_runs_the_test_targets_and_reports_each() {
    let p = project_with_tests("test-run");
    let r = p.run(".", &["test", "calc:unit"]);
    r.success();
    r.stderr_contains("test calc:unit ... ok");
    r.stderr_contains("test result: ok. 1 passed; 0 failed");
}

#[test]
fn a_failing_test_exits_nonzero_and_shows_its_output() {
    let p = project_with_tests("test-fail");
    let r = p.run(".", &["test"]);
    r.failure();
    // 通ったものと落ちたものが区別できる。
    r.stderr_contains("test calc:unit ... ok");
    r.stderr_contains("test calc:broken ... FAILED");
    // 失敗の理由と、そのテストの出力が見える。
    r.stderr_contains("exited with status 1");
    r.stderr_contains("expected 6, got 5");
    r.stderr_contains("test result: FAILED. 1 passed; 1 failed");
    // 通ったテストの出力は雑音なので出さない。
    assert!(!r.stderr.contains("sum=5"), "output of a passing test leaked\n{r}");
}

#[test]
fn no_run_builds_the_tests_without_starting_them() {
    let p = project_with_tests("test-no-run");
    let r = p.run(".", &["test", "--no-run"]);
    // 落ちるテストがあっても、走らせなければ成功で終わる。
    r.success();
    r.stderr_contains("built:");
    assert!(!r.stderr.contains("test result:"), "the tests were run\n{r}");
    assert!(build_dir(&p.root, "debug").join("bin/unit").exists());
}

#[test]
fn nocapture_lets_test_output_through() {
    let p = project_with_tests("test-nocapture");
    let r = p.run(".", &["test", "calc:unit", "--nocapture"]);
    r.success();
    // 素通しなので、通ったテストの出力も見える。
    r.stdout_contains("sum=5");
}

#[test]
fn test_results_are_available_as_json() {
    let p = project_with_tests("test-json");
    let r = p.run(".", &["test", "--message-format=json"]);
    r.failure();
    let lines: Vec<&str> = r.stdout.lines().filter(|l| l.contains("test-result")).collect();
    assert_eq!(lines.len(), 2, "expected one JSON object per test\n{r}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(r#""target":"calc:unit""#) && l.contains(r#""passed":true"#)),
        "{r}"
    );
    assert!(
        lines.iter().any(|l| l.contains(r#""target":"calc:broken""#)
            && l.contains(r#""passed":false"#)
            && l.contains(r#""exit_status":1"#)),
        "{r}"
    );
}

#[test]
fn test_refuses_a_target_that_is_not_a_test() {
    let p = project_with_tests("test-wrong-kind");
    let r = p.run(".", &["test", "calc:calc"]);
    r.failure();
    r.stderr_contains("is a lib target, not a test");
}

#[test]
fn test_says_so_when_there_is_nothing_to_run() {
    let p = Project::new("test-none");
    p.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    p.write("dowel.build", "[lib.a]\nsources = glob(\"*.c\")\n");
    p.write("a.c", "int a(void) { return 1; }\n");
    let r = p.run(".", &["test"]);
    r.success();
    r.stderr_contains("no test targets");
}

/// 最初に落ちるテストを置いた構成。`--fail-fast` の検査に使う。
fn project_with_a_failing_test_first(name: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"seq\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        r#"
[test.a_fails]
sources = glob("tests/a.c")

[test.b_passes]
sources = glob("tests/b.c")

[test.c_passes]
sources = glob("tests/c.c")
"#,
    );
    p.write("tests/a.c", "int main(void) { return 1; }\n");
    p.write("tests/b.c", "int main(void) { return 0; }\n");
    p.write("tests/c.c", "int main(void) { return 0; }\n");
    p
}

#[test]
fn fail_fast_stops_at_the_first_failure_and_says_what_was_skipped() {
    let p = project_with_a_failing_test_first("test-fail-fast");
    let r = p.run(".", &["test", "--fail-fast"]);
    r.failure();
    r.stderr_contains("test seq:a_fails ... FAILED");
    // 走らせなかった分を隠さない。
    r.stderr_contains("test result: FAILED. 0 passed; 1 failed; 2 not run");
    assert!(!r.stderr.contains("seq:b_passes ..."), "a later test was started\n{r}");

    // 既定は打ち切らない。全体像が要るため。
    let all = p.run(".", &["test"]);
    all.failure();
    all.stderr_contains("test seq:b_passes ... ok");
    all.stderr_contains("test result: FAILED. 2 passed; 1 failed");
    assert!(!all.stderr.contains("not run"), "{all}");
}

#[test]
fn failed_reruns_only_what_failed_last_time() {
    let p = project_with_tests("test-rerun");
    p.run(".", &["test"]).failure();

    let r = p.run(".", &["test", "--failed"]);
    r.failure();
    r.stderr_contains("running 1 test");
    r.stderr_contains("test calc:broken ... FAILED");
    assert!(!r.stderr.contains("calc:unit"), "a passing test was rerun\n{r}");

    // 直せば次の --failed には残らない。走らせていない calc:unit の判定も消えない。
    p.write(
        "tests/broken.c",
        "#include \"calc.h\"\nint main(void) { return add(2, 3) == 5 ? 0 : 1; }\n",
    );
    p.run(".", &["test", "--failed"]).success();
    let again = p.run(".", &["test", "--failed"]);
    again.success();
    again.stderr_contains("nothing to rerun");
}

#[test]
fn failed_says_so_when_there_is_no_record() {
    let p = project_with_tests("test-rerun-empty");
    let r = p.run(".", &["test", "--failed"]);
    r.success();
    r.stderr_contains("nothing to rerun");
}

#[test]
fn test_jobs_runs_several_at_once_and_keeps_the_requested_order() {
    let p = project_with_a_failing_test_first("test-jobs");
    let r = p.run(".", &["test", "--test-jobs=3"]);
    r.failure();
    // 並列でも表示は要求順。走った順に混ざらない。
    let order: Vec<&str> = r
        .stderr
        .lines()
        .filter(|l| l.starts_with("test seq:"))
        .map(|l| l.split_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(order, vec!["seq:a_fails", "seq:b_passes", "seq:c_passes"], "{r}");
    r.stderr_contains("test result: FAILED. 2 passed; 1 failed");
}

#[test]
fn nocapture_forces_one_test_at_a_time() {
    let p = project_with_tests("test-nocapture-jobs");
    // 素通しでの並列は出力が混ざるため、黙って直さず断りを入れて逐次にする。
    let r = p.run(".", &["test", "calc:unit", "--nocapture", "--test-jobs=4"]);
    r.success();
    r.stderr_contains("`--nocapture` forces one test at a time");
}

#[test]
fn diagnostics_carry_a_location_and_a_suggestion() {
    let p = Project::new("diagnostics");
    p.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.a]\nsources = glob(\"*.c\")\n\n[lib.a.public]\ninclude = [dir(\"x\")]\n",
    );
    // `check` は計画段まで走る（ADR-0010）。ソースを置かないと `empty-glob` が
    // 加わり、この事例が見たい1件だけの状態でなくなる。
    p.write("a.c", "int a(void) { return 0; }\n");
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
    assert_eq!(j.stdout.lines().count(), 1, "expected exactly one diagnostic per line\n{j}");
}

#[test]
fn semantic_diagnostics_survive_a_syntax_error() {
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
fn why_traces_the_propagation_path() {
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
fn the_graph_dumps_in_three_formats() {
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
fn schema_dump_works_without_a_manifest() {
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
fn an_abi_label_mismatch_stops_before_building() {
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
fn a_version_dependency_that_pkg_config_cannot_find_is_refused() {
    let p = Project::new("pkgconfig-missing");
    p.write(
        "dowel.toml",
        "[package]\nname = \"p\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"no-such-module-x9dowel\"\nversion = \"1.3\"\n",
    );
    p.write("dowel.build", "[bin.app]\nsources = glob(\"*.c\")\n");
    p.write("main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unsatisfied-dependency");
    r.stderr_contains("pkg-config");
}

#[test]
fn logs_expose_the_dependency_graph_and_action_counts() {
    let p = two_package_project("logging");
    let r = p.run("app", &["build", "--log-level=trace"]);
    r.success();
    r.stderr_contains("dependency graph:");
    r.stderr_contains("app:app → libfoo:foo");
    r.stderr_contains("actions (");

    // デバッグ用のトレース。「なぜこの引数になったのか」を追うために要る材料。
    r.stderr_contains("glob("); // ファイル掃引: 何件走査して何件一致したか
    r.stderr_contains("glob match "); // 一致したファイル
    r.stderr_contains("glob skip  "); // 走査に載ったが一致しなかったファイル
    r.stderr_contains("topological order:");
    r.stderr_contains("compile_env app:app.includes"); // 併合の入力数と結果
    r.stderr_contains("match cfg.opt"); // どのアームを選んだか
    r.stderr_contains("  include "); // 解決済みのインクルード探索パス
    r.stderr_contains("  define  "); // 解決済みの定義
    r.stderr_contains("  action["); // 各アクションの完全なコマンド列

    // 増分エンジンの観測。鍵は `FileId` でしか語れないため、
    // ファイル番号とパスの対応も同じログに出ていること。
    r.stderr_contains("parsing file ");
    r.stderr_contains("evaluating file ");
    r.stderr_contains("recomputed, value changed");
    r.stderr_contains(" is "); // `file 0 is <path>`
    r.stderr_contains("queries: "); // 計算・再利用・検証の内訳

    // 段階ごとの所要時間が出る。
    r.stderr_contains("← plan");
    r.stderr_contains("← execute");

    // JSON 形式のログは1行1オブジェクト。
    let j = p.run("app", &["check", "--log-level=debug", "--log-format=json"]);
    j.success();
    let line = j.stderr.lines().find(|l| l.contains("\"level\"")).expect("no JSON log line found");
    assert!(line.starts_with('{') && line.ends_with('}'), "{line}");
}

// --- ランナー抽象（docs/30-devexp.md 1節）--------------------------------

/// ホストのトリプル。`[runner.<triple>]` を実際に引かせるために要る。
fn host_triple() -> String {
    let p = Project::new("triple-probe");
    p.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    p.write("dowel.build", "[lib.a]\nsources = glob(\"*.c\")\n");
    p.write("a.c", "int a(void) { return 1; }\n");
    p.run(".", &["build"]).success();
    // ビルドディレクトリ名は `<triple>-<opt>`。
    let dir = build_dir(&p.root, "debug");
    let name = dir.file_name().expect("no build directory").to_string_lossy().to_string();
    name.trim_end_matches("-debug").to_string()
}

/// ランナーを1つ宣言したプロジェクト。テスト本体は環境変数を見て合否を決める。
///
/// qemu を要求せずにランナーの経路を通すため、ラッパには `env` を使う。
/// 「ラッパ経由で起動されたこと」がテスト自身から観測できる形にしてある。
fn project_with_a_runner(name: &str, triple: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"r\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        &format!(
            "[test.wrapped]\nsources = glob(\"*.c\")\n\n\
             [runner.{triple}]\ncommand = \"env\"\nargs    = [\"DOWEL_RAN_VIA_RUNNER=1\"]\n"
        ),
    );
    p.write(
        "wrapped.c",
        "#include <stdlib.h>\n\
         int main(void) { return getenv(\"DOWEL_RAN_VIA_RUNNER\") ? 0 : 1; }\n",
    );
    p
}

#[test]
fn a_declared_runner_wraps_the_test_binary() {
    let triple = host_triple();
    let p = project_with_a_runner("runner-wraps", &triple);

    let r = p.run(".", &["test"]);
    // ラッパ経由で起動されていなければ、テスト本体が終了状態 1 を返す。
    r.success();
    r.stderr_contains("test r:wrapped ... ok");
}

#[test]
fn the_runner_command_shows_up_in_the_trace() {
    let triple = host_triple();
    let p = project_with_a_runner("runner-trace", &triple);
    let r = p.run(".", &["test", "--log-level=trace"]);
    r.success();
    r.stderr_contains("declared runner for");
    // 何で包んだかが読める。ここが読めないと、クロス実行の失敗を切り分けられない。
    r.stderr_contains("runner for");
    r.stderr_contains("env DOWEL_RAN_VIA_RUNNER=1");
}

#[test]
fn without_a_runner_a_foreign_target_is_refused_before_launching() {
    // 起動後では `Exec format error` になり、構成の誤りが
    // テストの失敗として報告される。
    let p = Project::new("runner-missing");
    // ツールチェーンの宣言が無いトリプルは、ビルドより前に拒まれる（issue #42）。
    // ここで見たいのはランナーの検査なので、ホストのコンパイラを
    // そのトリプル向けとして宣言し、ビルドは通す。
    p.write(
        "dowel.toml",
        "[package]\nname    = \"r\"\nversion = \"0.1.0\"\n\n\
         [toolchain.riscv64gc-unknown-linux-gnu]\nc = \"cc\"\n",
    );
    p.write("dowel.build", "[test.t]\nsources = glob(\"*.c\")\n");
    p.write("t.c", "int main(void) { return 0; }\n");

    // ビルドは宣言されたコンパイラで通るが、起動は拒まれる。
    let r = p.run(".", &["test", "--target=riscv64gc-unknown-linux-gnu", "--no-run"]);
    r.success();
    let r = p.run(".", &["test", "--target=riscv64gc-unknown-linux-gnu"]);
    r.failure();
    r.stderr_contains("missing-runner");
    r.stderr_contains("riscv64gc-unknown-linux-gnu");
}

// --- ターゲットごとのツールチェーン（issue #42）---------------------------

#[test]
fn a_target_without_a_declared_toolchain_is_refused_before_building() {
    // ホストのコンパイラで組んで別トリプルの名前を付けると、誤りは
    // qemu の `Invalid ELF image` などとして1段あとに現れる。
    // 宣言が無いことを、組む前に宣言の不足として述べる。
    let p = Project::new("toolchain-missing");
    p.write("dowel.toml", "[package]\nname    = \"t\"\nversion = \"0.1.0\"\n");
    p.write("dowel.build", "[bin.t]\nsources = glob(\"*.c\")\n");
    p.write("t.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["build", "--target=riscv64gc-unknown-linux-gnu"]);
    r.failure();
    r.stderr_contains("missing-toolchain");
    r.stderr_contains("riscv64gc-unknown-linux-gnu");
    // 何も組まれていないこと。
    assert!(
        !p.path(".dowel").join("build").exists(),
        "artifacts were produced for a refused target"
    );
}

#[test]
fn a_toolchain_declared_for_the_target_triple_is_used() {
    // `[toolchain.<triple>]` が `--target` に追随することを、宣言した
    // コンパイラが実際に呼ばれることで確かめる。本物のクロスコンパイラを
    // 要求せず、呼ばれたことを記録するラッパを使う。
    let p = Project::new("toolchain-cross");
    let marker = p.path("cc-was-called");
    let wrapper = p.path("fake-cc");
    std::fs::write(&wrapper, format!("#!/bin/sh\ntouch {}\nexec cc \"$@\"\n", marker.display()))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // 無印の `[toolchain]` も並べ、別トリプルのビルドがそちらへ
    // 落ちないこと（そして mismatch 警告が出ないこと）を同時に見る。
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"t\"\nversion = \"0.1.0\"\n\n\
             [toolchain]\nc = \"cc\"\n\n\
             [toolchain.riscv64gc-unknown-linux-gnu]\nc = \"{}\"\n",
            wrapper.display()
        ),
    );
    p.write("dowel.build", "[bin.t]\nsources = glob(\"*.c\")\n");
    p.write("t.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["build", "--target=riscv64gc-unknown-linux-gnu"]);
    r.success();
    assert!(marker.exists(), "the declared cross toolchain was not invoked\n{r}");
    assert!(
        !r.stderr.contains("toolchain-mismatch"),
        "the host declaration was compared against the cross build\n{r}"
    );
    // 成果物はそのトリプルの名前のディレクトリに置かれる。
    let bin = p.path(".dowel/build/riscv64gc-unknown-linux-gnu-debug/bin/t");
    assert!(bin.exists(), "the artifact is missing: {}", bin.display());
}

#[test]
fn a_target_toolchain_without_a_c_compiler_is_refused() {
    // トリプル向けの宣言は、そのトリプルのビルド全体を担う。`c` が無いと
    // C のコンパイルがホストの既定へ落ち、成果物のアーキテクチャが
    // 黙って食い違う。
    let p = Project::new("toolchain-no-c");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"t\"\nversion = \"0.1.0\"\n\n\
         [toolchain.riscv64gc-unknown-linux-gnu]\ncxx = \"c++\"\n",
    );
    p.write("dowel.build", "[bin.t]\nsources = glob(\"*.c\")\n");
    p.write("t.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("missing-field");
    r.stderr_contains("has no `c`");
}

#[test]
fn the_declared_archiver_is_used_and_changing_it_rebuilds_the_archive() {
    // 書庫の作成もツールチェーンの一部である（issue #50）。宣言した
    // archiver が実際に呼ばれることと、宣言を変えると書庫が作り直される
    // ことを、呼ばれたことを記録するラッパで確かめる。
    let p = Project::new("toolchain-ar");
    let marker = p.path("ar-was-called");
    let wrapper = p.path("fake-ar");
    std::fs::write(&wrapper, format!("#!/bin/sh\ntouch {}\nexec ar \"$@\"\n", marker.display()))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = |ar: &str| {
        format!("[package]\nname    = \"t\"\nversion = \"0.1.0\"\n\n[toolchain]\nar = \"{ar}\"\n")
    };
    p.write("dowel.toml", &manifest("ar"));
    p.write(
        "dowel.build",
        "[lib.x]\nsources = glob(\"src/x.c\")\n\n\
         [bin.t]\nsources = glob(\"src/main.c\")\n\n[bin.t.private]\ndeps = [target(\"x\")]\n",
    );
    p.write("src/x.c", "int x_answer(void) { return 7; }\n");
    p.write(
        "src/main.c",
        "#include <stdio.h>\nint x_answer(void);\nint main(void) { printf(\"%d\\n\", x_answer()); return 0; }\n",
    );

    // 既定の `ar` で一度組む。宣言はまだラッパを指していない。
    p.run(".", &["build"]).success();
    assert!(!marker.exists(), "the wrapper ran before it was declared");

    // 宣言をラッパへ変えると、書庫のコマンドラインが変わり、組み直される。
    p.write("dowel.toml", &manifest(&wrapper.display().to_string()));
    p.run(".", &["build"]).success();
    assert!(marker.exists(), "the declared archiver was not invoked after the change");
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/t")), "7\n");
}

#[test]
fn a_misspelled_toolchain_key_is_refused_with_a_suggestion() {
    // 黙って無視すると、クロスの archiver の綴り間違いが既定値（ホストの
    // `ar`）への無言の後退になる——#50 が防ごうとした状態が戻る（issue #59）。
    let p = Project::new("toolchain-typo");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"t\"\nversion = \"0.1.0\"\n\n\
         [toolchain.aarch64-unknown-linux-gnu]\nc   = \"aarch64-linux-gnu-gcc\"\nar_ = \"aarch64-linux-gnu-ar\"\n",
    );
    p.write("dowel.build", "[bin.t]\nsources = glob(\"*.c\")\n");
    p.write("t.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-property");
    r.stderr_contains("did you mean `ar`?");
    r.stderr_contains("accepts: c, cxx, ar");
}

#[test]
fn a_missing_archiver_is_refused_only_when_an_archive_is_needed() {
    // 実在検査はコンパイラと同じく計画段で行う。ただし書庫を作らない
    // ビルドには要求しない（C++ ツールチェーンと同じ扱い）。
    let p = Project::new("toolchain-ar-missing");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"t\"\nversion = \"0.1.0\"\n\n[toolchain]\nar = \"no-such-ar-x9\"\n",
    );
    p.write("dowel.build", "[bin.t]\nsources = glob(\"*.c\")\n");
    p.write("t.c", "int main(void) { return 0; }\n");

    // bin だけなら書庫は要らず、宣言が悪くても通る。
    p.run(".", &["build"]).success();

    // lib が加わると計画段で拒まれる。
    p.write(
        "dowel.build",
        "[lib.x]\nsources = glob(\"*.c\")\n\n[bin.t]\nsources = glob(\"*.c\")\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("missing-toolchain");
    r.stderr_contains("no-such-ar-x9");
}

#[test]
fn a_runner_for_another_triple_is_not_used_for_the_host() {
    let p = Project::new("runner-other-triple");
    p.write("dowel.toml", "[package]\nname    = \"r\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        "[test.t]\nsources = glob(\"*.c\")\n\n\
         [runner.riscv64gc-unknown-linux-gnu]\ncommand = \"definitely-not-a-program\"\n",
    );
    p.write("t.c", "int main(void) { return 0; }\n");

    // ホスト構成では引かれないため、そのまま起動されて通る。
    p.run(".", &["test"]).success();
}

#[test]
fn a_runner_transfers_the_artifact_and_runs_the_transferred_copy() {
    // 実機や qemu-system を用意せずに転送の経路を検査するため、
    // 転送コマンドに `cp`、転送先にローカルのディレクトリを使う。
    // テスト本体は argv[0] を見て、転送先から起動されたことを確かめる。
    let triple = host_triple();
    let p = Project::new("runner-transfer");
    let staged = p.path("staged");
    std::fs::create_dir_all(&staged).unwrap();

    p.write("dowel.toml", "[package]\nname    = \"r\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        &format!(
            "[test.moved]\nsources = glob(\"*.c\")\n\n\
             [runner.{triple}]\n\
             transfer   = [\"cp\"]\n\
             remote_dir = \"{}\"\n\
             command    = \"env\"\n\
             args       = [\"DOWEL_VIA_RUNNER=1\"]\n",
            staged.display()
        ),
    );
    p.write(
        "moved.c",
        "#include <string.h>\n#include <stdlib.h>\n\
         int main(int argc, char **argv)\n\
         {\n\
         \x20   if (argc < 1 || !strstr(argv[0], \"staged/\")) return 1;\n\
         \x20   return getenv(\"DOWEL_VIA_RUNNER\") ? 0 : 2;\n\
         }\n",
    );

    let r = p.run(".", &["test"]);
    r.success();
    r.stderr_contains("test r:moved ... ok");
    assert!(staged.join("moved").exists(), "the artifact was not transferred");
}

#[test]
fn a_failed_transfer_is_reported_as_such_and_not_as_a_test_failure() {
    // 転送の失敗はテストの失敗ではない。原因を取り違えると、
    // 利用者はテスト対象のコードを調べることになる。
    let triple = host_triple();
    let p = Project::new("runner-transfer-fails");
    p.write("dowel.toml", "[package]\nname    = \"r\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        &format!(
            "[test.t]\nsources = glob(\"*.c\")\n\n\
             [runner.{triple}]\n\
             transfer   = [\"definitely-not-a-transfer-tool\"]\n\
             remote_dir = \"/tmp/dowel-nowhere\"\n\
             command    = \"env\"\n"
        ),
    );
    p.write("t.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("could not transfer the artifact");
    r.stderr_contains("definitely-not-a-transfer-tool");
}

#[test]
fn transfer_and_remote_dir_must_be_declared_together() {
    let p = Project::new("runner-transfer-incomplete");
    p.write("dowel.toml", "[package]\nname    = \"r\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        "[test.t]\nsources = glob(\"*.c\")\n\n\
         [runner.riscv64gc-unknown-linux-gnu]\ncommand = \"ssh\"\ntransfer = [\"scp\"]\n",
    );
    p.write("t.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("incomplete-runner");
    r.stderr_contains("remote_dir");
}

// --- ストア（docs/20-architecture.md 5節）--------------------------------

#[test]
fn cache_info_reports_an_empty_store_before_anything_is_written() {
    let p = two_package_project("cache-info");
    let r = p.run("app", &["cache", "info"]);
    r.success();
    r.stdout_contains("records    0");
    r.stdout_contains(".dowel/cache/v1");
}

#[test]
fn cache_gc_removes_stores_left_by_older_formats() {
    let p = two_package_project("cache-gc");
    // 過去の形式が残っている状態を作る。
    let old = p.path("app/.dowel/cache/v0");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("index"), b"stale").unwrap();

    let r = p.run("app", &["cache", "gc"]);
    r.success();
    r.stderr_contains("removed 1 store");
    assert!(!old.exists(), "the old store was not removed");

    // 2度目は何も消さない。
    p.run("app", &["cache", "gc"]).success().stderr_contains("removed 0 store");
}

#[test]
fn cache_commands_work_without_a_readable_manifest() {
    // 壊れたマニフェストの状態でも掃除できる必要がある。
    let p = Project::new("cache-broken-manifest");
    p.write("dowel.toml", "[package\n");
    p.run(".", &["cache", "info"]).success();
    p.run(".", &["cache", "gc"]).success();
}

/// `.c` と `.cpp` が同じターゲットに混在する1パッケージ。
///
/// コンパイラはソースごとに拡張子で選ばれ、リンクは C++ の driver で行われる。
/// `std::string` を経由させることで、C++ 標準ライブラリが実際にリンクされた
/// ことを実行結果まで通して確かめる。
#[test]
fn a_mixed_c_and_cxx_target_builds_and_runs() {
    let p = Project::new("cxx-mixed");
    p.write("app/dowel.toml", "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n");
    // `*.c*` が `.c` と `.cpp` の双方を拾う（`.h` は拾わない）。
    p.write(
        "app/dowel.build",
        r#"
[bin.app]
sources = glob("src/*.c*")

[bin.app.private]
includes = [dir("src")]
"#,
    );
    p.write("app/src/greet.h", "#pragma once\n#ifdef __cplusplus\nextern \"C\"\n#endif\nint greet_len(const char *name);\n");
    p.write(
        "app/src/greet.cpp",
        r#"#include "greet.h"
#include <string>
extern "C" int greet_len(const char *name) {
    std::string s("hello ");
    s += name;
    return (int)s.size();
}
"#,
    );
    p.write(
        "app/src/main.c",
        r#"#include <stdio.h>
#include "greet.h"
int main(void) {
    printf("len=%d\n", greet_len("cxx"));
    return 0;
}
"#,
    );

    p.run("app", &["check"]).success();
    let r = p.run("app", &["build"]);
    r.success();
    // 各ソースが自分の言語の driver でコンパイルされている。
    r.stderr_contains("CXX ");
    r.stderr_contains("CC ");
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "len=9\n");
}

/// C の main が C++ 実装の静的ライブラリに依存する2パッケージ。
///
/// リンク対象自身は C だけでも、閉包に C++ の翻訳単位があればリンクは
/// C++ の driver で行われなければならない。C の driver のままだと
/// `std::string` の未定義参照でリンクに失敗する。
#[test]
fn a_c_binary_linking_a_cxx_library_links_with_the_cxx_driver() {
    let p = Project::new("cxx-dep");
    p.write("liblen/dowel.toml", "[package]\nname    = \"liblen\"\nversion = \"0.1.0\"\n");
    p.write(
        "liblen/dowel.build",
        r#"
[lib.len]
sources = glob("src/*.cpp")

[lib.len.public]
includes = [dir("include")]
"#,
    );
    p.write(
        "liblen/include/len.h",
        "#pragma once\n#ifdef __cplusplus\nextern \"C\"\n#endif\nint len_of(const char *s);\n",
    );
    p.write(
        "liblen/src/len.cpp",
        r#"#include "len.h"
#include <string>
extern "C" int len_of(const char *s) {
    return (int)std::string(s).size();
}
"#,
    );
    p.write(
        "app/dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"liblen\"\npath = \"../liblen\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"liblen\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"len.h\"\nint main(void) { printf(\"n=%d\\n\", len_of(\"abcd\")); return 0; }\n",
    );

    // 双方の実行器で同じ結果になる。
    p.run("app", &["build", "--backend=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "n=4\n");

    p.run("app", &["build"]).success();
    assert_eq!(run_artifact(&bin), "n=4\n");
}

/// `[toolchain] cxx` の宣言が C++ のコンパイルとリンクの双方に効く。
#[test]
fn the_declared_cxx_toolchain_is_used_for_compile_and_link() {
    let p = Project::new("cxx-toolchain");
    p.write(
        "app/dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[toolchain]\ncxx = \"no-such-cxx-19\"\n",
    );
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.cpp\")\n");
    p.write("app/src/main.cpp", "int main() { return 0; }\n");

    // 実在しない C++ コンパイラは、起動前に計画段の診断で拒まれる。
    let r = p.run("app", &["check"]);
    r.failure();
    assert!(r.stderr.contains("missing-toolchain"), "{r}");
    assert!(r.stderr.contains("no-such-cxx-19"), "{r}");

    // C だけのビルドは C++ ツールチェーンを要求しない。
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    std::fs::remove_file(p.path("app/src/main.cpp")).unwrap();
    p.run("app", &["check"]).success();
}

/// git 依存の「上流」として使うリポジトリを作り、その commit sha を返す。
///
/// 内容は C の静的ライブラリ1つ。取得側はこれを `git = <パス>` と
/// フル 40 桁の `rev` で固定する。
fn git_remote(p: &Project) -> String {
    p.write("remote/dowel.toml", "[package]\nname    = \"liblen\"\nversion = \"0.1.0\"\n");
    p.write(
        "remote/dowel.build",
        "[lib.len]\nsources = glob(\"src/*.c\")\n\n[lib.len.public]\nincludes = [dir(\"include\")]\n",
    );
    p.write("remote/include/len.h", "#pragma once\nint len_of(const char *s);\n");
    p.write(
        "remote/src/len.c",
        "#include \"len.h\"\nint len_of(const char *s) { int n = 0; while (s[n]) n++; return n; }\n",
    );
    let dir = p.path("remote");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("cannot run git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    git(&["init", "--quiet"]);
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "initial"]);
    git(&["rev-parse", "HEAD"]).trim().to_string()
}

/// app の `dowel.toml` を、上流を rev で固定する形で書く。
fn write_git_manifest(p: &Project, rev: &str) {
    p.write(
        "app/dowel.toml",
        &format!(
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"liblen\"\ngit  = \"{}\"\nrev  = \"{rev}\"\n",
            p.path("remote").display()
        ),
    );
}

/// 取得からビルド・実行までと、取得済み checkout のオフライン再利用。
#[test]
fn a_pinned_git_dependency_is_fetched_and_reused_offline() {
    let p = Project::new("git-dep");
    let rev = git_remote(&p);
    write_git_manifest(&p, &rev);
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"liblen\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"len.h\"\nint main(void) { printf(\"n=%d\\n\", len_of(\"abc\")); return 0; }\n",
    );

    p.run("app", &["build"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "n=3\n");

    // checkout は `.dowel/deps/<name>-<rev12>/` に置かれ、完了印を持つ。
    let checkout = p.path("app/.dowel/deps").join(format!("liblen-{}", &rev[..12]));
    assert!(checkout.join(".dowel-rev").exists(), "missing {}", checkout.display());

    // 上流を消しても再ビルドできる。rev が固定されているため、
    // 2回目以降の解決はネットワーク（ここでは上流のパス）に触れない。
    std::fs::remove_dir_all(p.path("remote")).unwrap();
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"len.h\"\nint main(void) { printf(\"n=%d\\n\", len_of(\"abcde\")); return 0; }\n",
    );
    p.run("app", &["build"]).success();
    assert_eq!(run_artifact(&bin), "n=5\n");
}

/// 実在する上流でも、そこに無い rev は取得の診断で拒まれる。
#[test]
fn an_unknown_rev_is_refused_with_a_diagnostic() {
    let p = Project::new("git-dep-bad-rev");
    let _ = git_remote(&p);
    write_git_manifest(&p, "0123456789abcdef0123456789abcdef01234567");
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");

    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("unfetchable-dependency");
    r.stderr_contains("liblen");
}

/// 言語別フラグ。`flags` は両言語、`c_flags` / `cxx_flags` は各言語にのみ効く。
///
/// 漏れの検査はフィクスチャ側の `#error` で行う（51-testing の方針どおり、
/// 期待値をハーネスに二重に持たない）。`-std=c++11` が実際に届いたことは
/// `__cplusplus` の static_assert が確かめる。
#[test]
fn per_language_flags_reach_only_their_language() {
    let p = Project::new("lang-flags");
    p.write("app/dowel.toml", "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n");
    p.write(
        "app/dowel.build",
        r#"
[bin.app]
sources = glob("src/*.c*")

[bin.app.private]
includes  = [dir("src")]
flags     = ["-DCOMMON=1"]
c_flags   = ["-DFROM_C=1"]
cxx_flags = ["-std=c++11", "-DFROM_CXX=1"]
"#,
    );
    p.write(
        "app/src/greet.h",
        "#pragma once\n#ifdef __cplusplus\nextern \"C\"\n#endif\nint cxx_part(void);\n",
    );
    p.write(
        "app/src/greet.cpp",
        r#"#include "greet.h"
static_assert(__cplusplus == 201103L, "cxx_flags did not reach the C++ compile");
#ifdef FROM_C
#error "c_flags leaked into a C++ translation unit"
#endif
#ifndef COMMON
#error "flags did not reach the C++ compile"
#endif
extern "C" int cxx_part(void) { return FROM_CXX; }
"#,
    );
    p.write(
        "app/src/main.c",
        r#"#include <stdio.h>
#include "greet.h"
#ifdef FROM_CXX
#error "cxx_flags leaked into a C translation unit"
#endif
#ifndef COMMON
#error "flags did not reach the C compile"
#endif
int main(void) { printf("c=%d cxx=%d\n", FROM_C, cxx_part()); return 0; }
"#,
    );

    p.run("app", &["build"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "c=1 cxx=1\n");
}

/// `dowel new` が生成した bin パッケージは、そのままビルドして実行できる。
///
/// 雛型が仕様の変更に置いていかれると、最初の体験が「生成物が壊れている」に
/// なる。生成 → ビルド → 実行を常時通すことで雛型の陳腐化を防ぐ。
#[test]
fn a_new_bin_package_builds_and_runs() {
    let p = Project::new("scaffold-bin");
    p.run(".", &["new", "myapp"]).success().stderr_contains("created bin package `myapp`");
    p.run("myapp", &["check"]).success();
    p.run("myapp", &["build"]).success();
    let bin = build_dir(&p.path("myapp"), "debug").join("bin/myapp");
    assert_eq!(run_artifact(&bin), "hello from myapp\n");

    // 生成先が空でなければ書かない。
    let r = p.run(".", &["new", "myapp"]);
    r.failure();
    assert!(r.stderr.contains("already"), "{r}");
}

/// `dowel new --lib` の雛型はテストつきで、`dowel test` がそのまま通る。
#[test]
fn a_new_lib_package_passes_its_own_test() {
    let p = Project::new("scaffold-lib");
    p.run(".", &["new", "mylib", "--lib"]).success();
    p.run("mylib", &["test"]).success();
}

/// `dowel add` はサブパッケージを作り、`dowel.toml` へ path 依存を追記する。
/// 追記後もマニフェストは厳密な TOML として読める。
#[test]
fn add_creates_a_subpackage_and_declares_the_dependency() {
    let p = Project::new("scaffold-add");
    p.run(".", &["new", "myapp"]).success();
    p.run("myapp", &["add", "libs/util"]).success().stderr_contains("declared it in");

    let manifest = std::fs::read_to_string(p.path("myapp/dowel.toml")).unwrap();
    assert!(manifest.contains("name = \"util\""), "{manifest}");
    assert!(manifest.contains("path = \"libs/util\""), "{manifest}");

    // 追記されたマニフェストのまま check が通り、依存も読める。
    p.run("myapp", &["check"]).success();

    // 案内どおり deps を配線すると、実際に使える。
    p.write(
        "myapp/dowel.build",
        "[bin.myapp]\nsources = glob(\"src/*.c\")\n\n[bin.myapp.private]\ndeps = [dep(\"util\")]\n",
    );
    p.write(
        "myapp/src/main.c",
        "#include <stdio.h>\n#include \"util.h\"\nint main(void) { printf(\"a=%d\\n\", util_answer()); return 0; }\n",
    );
    p.run("myapp", &["build"]).success();
    let bin = build_dir(&p.path("myapp"), "debug").join("bin/myapp");
    assert_eq!(run_artifact(&bin), "a=42\n");

    // 同じ名前の二重宣言は拒む。
    let r = p.run("myapp", &["add", "other/util"]);
    r.failure();
    assert!(r.stderr.contains("already declared"), "{r}");
}

/// `dowel add --git` は git 依存を宣言する。書かれるのはフル 40 桁の sha のみ。
/// rev を省いた場合は HEAD を `git ls-remote` で一度だけ解決して固定する。
#[test]
fn add_git_declares_a_pinned_dependency() {
    let p = Project::new("scaffold-add-git");
    let rev = git_remote(&p);
    let url = p.path("remote").display().to_string();
    p.run(".", &["new", "myapp"]).success();

    // 明示した sha はそのまま書かれる。
    p.run("myapp", &["add", "--git", &url, "--rev", &rev, "--name", "liblen"])
        .success()
        .stderr_contains("declared git dependency `liblen`");
    let manifest = std::fs::read_to_string(p.path("myapp/dowel.toml")).unwrap();
    assert!(manifest.contains(&format!("rev  = \"{rev}\"")), "{manifest}");

    // 宣言のまま check が通り、取得も走る。配線して実際に使える。
    p.write(
        "myapp/dowel.build",
        "[bin.myapp]\nsources = glob(\"src/*.c\")\n\n[bin.myapp.private]\ndeps = [dep(\"liblen\")]\n",
    );
    p.write(
        "myapp/src/main.c",
        "#include <stdio.h>\n#include \"len.h\"\nint main(void) { printf(\"n=%d\\n\", len_of(\"ab\")); return 0; }\n",
    );
    p.run("myapp", &["build"]).success();
    let bin = build_dir(&p.path("myapp"), "debug").join("bin/myapp");
    assert_eq!(run_artifact(&bin), "n=2\n");

    // rev を省くと HEAD を解決して固定する。名前は URL の最終要素から取る。
    p.run(".", &["new", "other"]).success();
    p.run("other", &["add", "--git", &url]).success();
    let manifest = std::fs::read_to_string(p.path("other/dowel.toml")).unwrap();
    assert!(manifest.contains("name = \"remote\""), "{manifest}");
    assert!(manifest.contains(&format!("rev  = \"{rev}\"")), "resolved HEAD differs\n{manifest}");
}

/// `migrate verify`。参照の compile_commands.json と計画の等価性を検査する。
///
/// dowel 自身の出力を参照に使えば完全一致になる（正規化の恒等性）。
/// 参照側の define を変えると、そのソースが差分として名指しされる。
/// 未移植（参照側にだけあるソース）は報告されるが失敗にはしない。
#[test]
fn migrate_verify_compares_against_a_reference_compdb() {
    let p = two_package_project("migrate-verify");
    p.run("app", &["build"]).success();
    let compdb = std::fs::read_to_string(p.path("app/compile_commands.json")).unwrap();

    // 自分自身とは等価。
    p.write("ref.json", &compdb);
    let r = p.run("app", &["migrate", "verify", "../ref.json"]);
    r.success();
    r.stdout_contains("2 equivalent, 0 differing, 0 not ported");

    // 構成のフラグは両側から等しく除かれる。参照が別の build type
    // （release 相当の -O2 -DNDEBUG）でも、それは移行の差ではない
    // （issue #54）。
    p.write("ref.json", &compdb.replace("\"cc\",", "\"cc\", \"-O2\", \"-DNDEBUG\","));
    let r = p.run("app", &["migrate", "verify", "../ref.json"]);
    r.success();
    r.stdout_contains("2 equivalent, 0 differing");

    // 参照側の define が違えば、そのソースと引数が名指しされて失敗する。
    p.write("ref.json", &compdb.replace("-DAPP_OPT=0", "-DAPP_OPT=9"));
    let r = p.run("app", &["migrate", "verify", "../ref.json"]);
    r.failure();
    r.stdout_contains("main.c");
    r.stdout_contains("-DAPP_OPT=9");
    r.stdout_contains("(in the reference, not in dowel)");

    // 未移植は途中経過であり、報告はするが失敗にしない。
    let unported = compdb.trim_end().trim_end_matches(']').to_string()
        + ",{\"directory\": \"/b\", \"file\": \"/old/legacy.c\", \"arguments\": [\"cc\", \"-c\", \"/old/legacy.c\"]}]";
    p.write("ref.json", &unported);
    let r = p.run("app", &["migrate", "verify", "../ref.json"]);
    r.success();
    r.stdout_contains("1 not ported");
    r.stdout_contains("legacy.c");

    // 機械可読の形も出る。
    let r = p.run("app", &["migrate", "verify", "../ref.json", "--format=json"]);
    r.success();
    r.stdout_contains("\"equivalent\"");
    r.stdout_contains("\"unported\"");
}

/// `migrate import`。CMake File API の reply から下書きを生成し、
/// そのままビルド・実行できることまで確かめる。
///
/// reply は手書きのフィクスチャで持つ。cmake の実行を要さず、
/// File API の形式（安定な JSON）に対する検査として十分である。
#[test]
fn migrate_import_drafts_manifests_from_a_cmake_reply() {
    let p = Project::new("cmake-import");
    let src = p.path(".").display().to_string();
    p.write("lib/len.h", "#pragma once\nint len_of(const char *s);\n");
    p.write(
        "lib/len.c",
        "#include \"len.h\"\nint len_of(const char *s) { int n = 0; while (s[n]) n++; return n + LIMIT - LIMIT; }\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\n#include \"len.h\"\nint main(void) { printf(\"n=%d\\n\", len_of(\"abcd\")); return 0; }\n",
    );

    let reply = "build/.cmake/api/v1/reply";
    p.write(
        &format!("{reply}/codemodel-v2-0000.json"),
        &format!(
            r#"{{"configurations": [{{"name": "Debug",
                 "projects": [{{"name": "demo"}}],
                 "targets": [{{"name": "len", "jsonFile": "target-len.json"}},
                             {{"name": "app", "jsonFile": "target-app.json"}}]}}],
                "paths": {{"source": "{src}", "build": "{src}/build"}}}}"#
        ),
    );
    p.write(
        &format!("{reply}/target-len.json"),
        &format!(
            r#"{{"name": "len", "type": "STATIC_LIBRARY",
                "sources": [{{"path": "lib/len.c"}}, {{"path": "lib/len.h"}}],
                "compileGroups": [{{"language": "C",
                    "defines": [{{"define": "LIMIT=64"}}],
                    "includes": [{{"path": "{src}/lib"}}],
                    "compileCommandFragments": [{{"fragment": "-Wall"}},
                                                {{"fragment": "-O3 -DNDEBUG -g"}}]}}]}}"#
        ),
    );
    p.write(
        &format!("{reply}/target-app.json"),
        &format!(
            r#"{{"name": "app", "type": "EXECUTABLE",
                "sources": [{{"path": "src/main.c"}}],
                "compileGroups": [{{"language": "C",
                    "includes": [{{"path": "{src}/lib"}}]}}],
                "dependencies": [{{"id": "len::@6890427a1f51a3e7e1df"}}],
                "link": {{"commandFragments": [{{"role": "libraries", "fragment": "-lm"}},
                                               {{"role": "flags", "fragment": "-O3 -DNDEBUG -g"}}]}}}}"#
        ),
    );

    let r = p.run(".", &["migrate", "import", "build"]);
    r.success();
    r.stderr_contains("imported 2 target(s)");
    r.stderr_contains("UNVERIFIED");

    // 生成物は未検証の印を持ち、意図の欠落と検証の導線を説明する。
    let build_file = std::fs::read_to_string(p.path("dowel.build")).unwrap();
    assert!(build_file.contains("UNVERIFIED DRAFT"), "{build_file}");
    assert!(build_file.contains("migrate verify"), "{build_file}");

    // 構成レベルのフラグ（build type 由来の -O / -g / -DNDEBUG）は写らない。
    // 写すと無条件のフラグになり、release から取り込んだ下書きの debug
    // ビルドが最適化された NDEBUG 付きになる（issue #54）。
    // ヘッダのコメントもその旨を述べる。
    assert!(build_file.contains("were NOT copied"), "{build_file}");
    let flags_line = build_file
        .lines()
        .find(|l| l.trim_start().starts_with("flags"))
        .expect("the draft declares flags");
    assert!(flags_line.contains("-Wall"), "{flags_line}");
    for dropped in ["-O3", "-DNDEBUG", "-g"] {
        assert!(!flags_line.contains(dropped), "`{dropped}` was copied: {flags_line}");
    }
    // リンク側（link.commandFragments）も同じ判定で落ちる。翻訳側だけを
    // 落とすと、見出しの「写していない」と中身が食い違う（issue #61）。
    let link_line = build_file
        .lines()
        .find(|l| l.trim_start().starts_with("link_flags"))
        .expect("the draft declares link_flags");
    assert!(link_line.contains("-lm"), "{link_line}");
    for dropped in ["-O3", "-DNDEBUG", "-g\""] {
        assert!(!link_line.contains(dropped), "`{dropped}` was copied: {link_line}");
    }

    // 下書きはそのまま計画・ビルド・実行まで通る。
    p.run(".", &["check"]).success();
    p.run(".", &["build"]).success();
    let bin = build_dir(&p.path("."), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "n=4\n");

    // 既存のマニフェストは上書きしない。
    let again = p.run(".", &["migrate", "import", "build"]);
    again.failure();
    assert!(again.stderr.contains("already exists"), "{again}");
}

/// pkg-config で解決する `version` 依存の全経路。
///
/// システムのモジュールに依存すると環境で揺れるため、`.pc` と実体の
/// 静的ライブラリをテスト自身が用意し、`PKG_CONFIG_PATH` で向ける。
/// `${pcfiledir}` 基点の .pc は場所に依らず成立する。
#[test]
fn a_version_dependency_resolves_through_pkg_config_and_locks() {
    let p = Project::new("pkgconfig-dep");
    // 「システムパッケージ」の実体を作る。ヘッダ + 静的ライブラリ + .pc。
    p.write("ext/include/mylib.h", "#pragma once\nint mylib_answer(void);\n");
    p.write("ext/mylib.c", "int mylib_answer(void) { return 42; }\n");
    let ext = p.path("ext");
    let sh = |cmd: &mut std::process::Command| {
        let out = cmd.current_dir(&ext).output().expect("cannot run the tool");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    sh(std::process::Command::new("cc").args(["-c", "mylib.c", "-o", "mylib.o"]));
    sh(std::process::Command::new("ar").args(["rcs", "libmylib.a", "mylib.o"]));
    p.write(
        "ext/mylib.pc",
        "Name: mylib\nDescription: test module\nVersion: 2.5.0\n\
         Cflags: -I${pcfiledir}/include\nLibs: -L${pcfiledir} -lmylib\n",
    );
    let pc_path = ext.display().to_string();
    let env: &[(&str, &str)] = &[("PKG_CONFIG_PATH", &pc_path)];

    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"mylib\"\nversion = \"2.0\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"mylib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include <mylib.h>\nint main(void) { printf(\"a=%d\\n\", mylib_answer()); return 0; }\n",
    );

    // 解決 → ビルド → 実行。cflags/libs が伝播していなければ通らない。
    p.run_env("app", &["build"], env).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "a=42\n");

    // 解決結果が dowel.lock に記録される。
    let lock = std::fs::read_to_string(p.path("app/dowel.lock")).unwrap();
    assert!(lock.contains("name    = \"mylib\""), "{lock}");
    assert!(lock.contains("version = \"2.5.0\""), "{lock}");
    assert!(lock.contains("source  = \"pkg-config\""), "{lock}");

    // 一致していれば静か。
    let r = p.run_env("app", &["check"], env);
    r.success();
    assert!(!r.stderr.contains("lockfile-drift"), "{r}");

    // 記録と食い違えば警告し、ロックは書き換えない。
    p.write("app/dowel.lock", &lock.replace("2.5.0", "9.9.9"));
    let r = p.run_env("app", &["check"], env);
    r.success();
    r.stderr_contains("lockfile-drift");
    r.stderr_contains("9.9.9");
    let after = std::fs::read_to_string(p.path("app/dowel.lock")).unwrap();
    assert!(after.contains("9.9.9"), "the lock was rewritten silently\n{after}");

    // 版の下限を満たさなければ失敗する。
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"mylib\"\nversion = \"3.0\"\n",
    );
    let r = p.run_env("app", &["check"], env);
    r.failure();
    r.stderr_contains("unsatisfied-dependency");
    r.stderr_contains("does not satisfy >= 3.0");
}

/// 外部の静的ライブラリを1つ作る。`-L<dir> -l<name>` で繋ぐ検査の材料。
fn build_external_lib(p: &Project, dir: &str, name: &str, source: &str) {
    p.write(&format!("{dir}/{name}.c"), source);
    let cwd = p.path(dir);
    let sh = |cmd: &mut std::process::Command| {
        let out = cmd.current_dir(&cwd).output().expect("cannot run the tool");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    sh(std::process::Command::new("cc").args(["-c", &format!("{name}.c"), "-o", "x.o"]));
    sh(std::process::Command::new("ar").args(["rcs", &format!("lib{name}.a"), "x.o"]));
}

#[test]
fn private_link_flags_of_a_library_ride_the_link_closure() {
    // 静的な書庫は自分のリンク要件を運べない。lib が private に持つ
    // link_flags は、書庫と同じくリンクの閉包（private の段を跨いで）を
    // 辿って最終リンクに乗る。閉包を辿らなければ undefined reference に
    // なる形として、外部の書庫を -L / -l で繋ぐ（issue #56）。
    let p = Project::new("private-link-flags");
    build_external_lib(&p, "ext", "extra", "int extra_answer(void) { return 40; }\n");

    // top → mid → leaf。private を2段跨ぐ。
    p.write(
        "top/dowel.toml",
        "[package]\nname = \"top\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"mid\"\npath = \"../mid\"\n",
    );
    p.write(
        "top/dowel.build",
        "[bin.top]\nsources = [file(\"src/main.c\")]\n\n[bin.top.private]\ndeps = [dep(\"mid\")]\n",
    );
    p.write(
        "top/src/main.c",
        "#include <stdio.h>\nint mid_value(void);\nint main(void) { printf(\"v=%d\\n\", mid_value()); return 0; }\n",
    );
    p.write(
        "mid/dowel.toml",
        "[package]\nname = \"mid\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"leaf\"\npath = \"../leaf\"\n",
    );
    p.write(
        "mid/dowel.build",
        "[lib.mid]\nsources = [file(\"src/mid.c\")]\n\n[lib.mid.private]\ndeps = [dep(\"leaf\")]\n",
    );
    p.write(
        "mid/src/mid.c",
        "int leaf_value(void);\nint mid_value(void) { return leaf_value() + 1; }\n",
    );
    p.write("leaf/dowel.toml", "[package]\nname = \"leaf\"\nversion = \"0\"\n");
    p.write(
        "leaf/dowel.build",
        &format!(
            "[lib.leaf]\nsources = [file(\"src/leaf.c\")]\n\n\
             [lib.leaf.private]\nlink_flags = [\"-L{}\", \"-lextra\"]\n",
            p.path("ext").display()
        ),
    );
    p.write(
        "leaf/src/leaf.c",
        "int extra_answer(void);\nint leaf_value(void) { return extra_answer() + 1; }\n",
    );

    p.run("top", &["build"]).success();
    let bin = build_dir(&p.path("top"), "debug").join("bin/top");
    assert_eq!(run_artifact(&bin), "v=42\n");
}

#[test]
fn a_private_system_dependency_of_a_library_still_links_its_dependent() {
    // mid（lib）が version 依存を private に使い、top（bin）が mid に依存する。
    // `--libs` はリンクの閉包を辿って top のリンクに乗り、`--cflags` は
    // private の意味どおり top の翻訳には届かない（issue #56）。
    // 「ヘッダを漏らさない」と「リンクできる」は同時に成り立つ。
    let p = Project::new("pkgconfig-private-dep");
    p.write("ext/include/demokit.h", "#pragma once\n#define DEMOKIT_ANSWER 41\n");
    build_external_lib(&p, "ext", "demokit", "int demokit_bonus(void) { return 1; }\n");
    p.write(
        "ext/demokit.pc",
        "Name: demokit\nDescription: fixture\nVersion: 2.4.0\n\
         Cflags: -I${pcfiledir}/include -DDEMOKIT=1\nLibs: -L${pcfiledir} -ldemokit\n",
    );
    let pc_path = p.path("ext").display().to_string();
    let env: &[(&str, &str)] = &[("PKG_CONFIG_PATH", &pc_path)];

    p.write(
        "top/dowel.toml",
        "[package]\nname = \"top\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"mid\"\npath = \"../mid\"\n",
    );
    p.write(
        "top/dowel.build",
        "[bin.top]\nsources = [file(\"src/main.c\")]\n\n[bin.top.private]\ndeps = [dep(\"mid\")]\n",
    );
    // private な依存の Cflags が top の翻訳へ漏れたら、ここで止まる。
    p.write(
        "top/src/main.c",
        "#include <stdio.h>\n\
         #ifdef DEMOKIT\n#error \"the private dependency's cflags leaked\"\n#endif\n\
         int mid_value(void);\nint main(void) { printf(\"v=%d\\n\", mid_value()); return 0; }\n",
    );
    p.write(
        "mid/dowel.toml",
        "[package]\nname = \"mid\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"demokit\"\nversion = \"2.0\"\n",
    );
    p.write(
        "mid/dowel.build",
        "[lib.mid]\nsources = [file(\"src/mid.c\")]\n\n[lib.mid.private]\ndeps = [dep(\"demokit\")]\n",
    );
    p.write(
        "mid/src/mid.c",
        "#include <demokit.h>\nint demokit_bonus(void);\n\
         int mid_value(void) { return DEMOKIT_ANSWER + demokit_bonus(); }\n",
    );

    p.run_env("top", &["build"], env).success();
    let bin = build_dir(&p.path("top"), "debug").join("bin/top");
    assert_eq!(run_artifact(&bin), "v=42\n");
}

/// `[<kind>.<name>.artifacts]` — 成果物から別の成果物を作る（issue #60）。
///
/// 組み込みで要るのは `objcopy -O binary app.elf app.bin` の形である。
/// 本物のクロス環境を要求せず、ホストの objcopy で同じ経路を通す。
#[test]
fn a_bin_target_can_derive_artifacts_with_objcopy() {
    if !program_exists("objcopy") {
        eprintln!("skipping: objcopy is not on PATH");
        return;
    }
    let p = Project::new("artifacts-objcopy");
    p.write("dowel.toml", "[package]\nname = \"fw\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.firmware]\nsources = glob(\"src/*.c\")\n\n\
         [bin.firmware.artifacts]\n\
         bin = { tool = \"objcopy\", args = [\"-O\", \"binary\"] }\n\
         hex = { tool = \"objcopy\", args = [\"-O\", \"ihex\"] }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["build"]);
    r.success();
    let dir = build_dir(&p.path("."), "debug");
    for derived in ["bin/firmware.bin", "bin/firmware.hex"] {
        let path = dir.join(derived);
        assert!(path.exists(), "{} was not produced\n{r}", path.display());
        assert!(std::fs::metadata(&path).unwrap().len() > 0, "{} is empty", path.display());
    }
    // 作ったものは述べる。述べないと `.bin` が出来ていることが見えない。
    r.stderr_contains("firmware.bin");

    // 元の成果物が変わらなければ作り直さない。
    let stamp = std::fs::metadata(dir.join("bin/firmware.bin")).unwrap().modified().unwrap();
    p.run(".", &["build"]).success();
    assert_eq!(
        std::fs::metadata(dir.join("bin/firmware.bin")).unwrap().modified().unwrap(),
        stamp,
        "the transform ran again although its input did not change"
    );

    // ソースを変えれば、リンクの後に変換も走り直す。
    p.write("src/main.c", "int main(void) { return 1; }\n");
    p.run(".", &["build"]).success();
    assert_ne!(
        std::fs::metadata(dir.join("bin/firmware.bin")).unwrap().modified().unwrap(),
        stamp,
        "the transform did not re-run after its input changed"
    );
}

#[test]
fn the_transform_tool_is_selected_by_the_toolchain_declaration() {
    // 変換の道具もトリプルごとに選べる。宣言した実体が呼ばれることを、
    // 呼ばれたことを記録するラッパで確かめる（issue #60）。
    let p = Project::new("artifacts-tool-selection");
    let marker = p.path("objcopy-was-called");
    let wrapper = p.path("fake-objcopy");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\ntouch {}\nshift 2\ncp \"$1\" \"$2\"\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"fw\"\nversion = \"0\"\n\n[toolchain]\nobjcopy = \"{}\"\n",
            wrapper.display()
        ),
    );
    p.write(
        "dowel.build",
        "[bin.firmware]\nsources = glob(\"src/*.c\")\n\n\
         [bin.firmware.artifacts]\nbin = { tool = \"objcopy\", args = [\"-O\", \"binary\"] }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    p.run(".", &["build"]).success();
    assert!(marker.exists(), "the declared objcopy was not invoked");
    assert!(build_dir(&p.path("."), "debug").join("bin/firmware.bin").exists());
}

#[test]
fn an_artifact_naming_a_tool_that_does_not_exist_is_refused() {
    // 実体の名前（`arm-none-eabi-objcopy`）を直に書くと、トリプルごとの選択も
    // 記録された入力も効かない。道具の名前でしか書けないことを述べる。
    let p = Project::new("artifacts-unknown-tool");
    p.write("dowel.toml", "[package]\nname = \"fw\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.firmware]\nsources = glob(\"src/*.c\")\n\n\
         [bin.firmware.artifacts]\nbin = { tool = \"arm-none-eabi-objcopy\" }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-tool");
    r.stderr_contains("declarable tools:");
    r.stderr_contains("`[toolchain]`");
}

/// PATH 上に道具が在るか。テストの前提を確かめるためだけの簡易版。
fn program_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .any(|dir| std::fs::metadata(dir.join(name)).map(|m| m.is_file()).unwrap_or(false))
        })
        .unwrap_or(false)
}

/// 型付きの言語標準（`c_std` / `cxx_std`）。
///
/// 要点は併合が `max` であることである。C++17 を要求するライブラリを
/// C++20 の実行ファイルから使う形は正しく、そこで落ちてはならない。
#[test]
fn the_language_standard_is_typed_and_the_highest_in_the_closure_wins() {
    let p = Project::new("cxx-std");
    // lib は C++17 を要求し、app は C++20 で組む。
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.cpp\")]\n\n\
         [bin.app.private]\ndeps    = [dep(\"lib\")]\ncxx_std = \"c++20\"\n",
    );
    // C++20 でしか通らない書き方（`consteval`）を使う。
    p.write(
        "app/src/main.cpp",
        "#include <cstdio>\nint lib_value();\n\
         consteval int twenty() { return 20; }\n\
         int main() { std::printf(\"v=%d\\n\", lib_value() + twenty()); return 0; }\n",
    );
    p.write("lib/dowel.toml", "[package]\nname = \"lib\"\nversion = \"0\"\n");
    p.write(
        "lib/dowel.build",
        "[lib.lib]\nsources = [file(\"src/lib.cpp\")]\n\n[lib.lib.public]\ncxx_std = \"c++17\"\n",
    );
    // C++17 でも C++20 でも通る書き方。lib 自身は宣言どおり C++17 で組まれる。
    p.write(
        "lib/src/lib.cpp",
        "#if __cplusplus < 201703L\n#error \"the declared standard did not reach the compiler\"\n#endif\n\
         int lib_value() { return 22; }\n",
    );

    p.run("app", &["build"]).success();
    assert_eq!(run_artifact(&build_dir(&p.path("app"), "debug").join("bin/app")), "v=42\n");

    // 実際に渡った `-std=` を compile_commands.json で確かめる。
    let compdb = std::fs::read_to_string(p.path("app/compile_commands.json")).unwrap();
    // app は自分の c++20。lib の c++17 が届いても下げられない。
    let app = compdb_entry(&compdb, "main.cpp");
    assert!(app.contains("-std=c++20"), "{app}");
    assert!(!app.contains("-std=c++17"), "{app}");
    // lib は自分の宣言どおり。
    assert!(compdb_entry(&compdb, "lib.cpp").contains("-std=c++17"));
}

#[test]
fn a_public_standard_raises_its_dependents() {
    // 逆向き: 依存の方が高い標準を要求する。使う側は引き上げられる。
    // 引き上げなければ、公開ヘッダの C++20 の記述が依存元で通らない。
    let p = Project::new("cxx-std-raise");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.cpp\")]\n\n\
         [bin.app.private]\ndeps    = [dep(\"lib\")]\ncxx_std = \"c++14\"\n",
    );
    p.write(
        "app/src/main.cpp",
        "#include <cstdio>\n\
         #if __cplusplus < 202002L\n#error \"the dependency's standard did not raise this target\"\n#endif\n\
         int lib_value();\nint main() { std::printf(\"v=%d\\n\", lib_value()); return 0; }\n",
    );
    p.write("lib/dowel.toml", "[package]\nname = \"lib\"\nversion = \"0\"\n");
    p.write(
        "lib/dowel.build",
        "[lib.lib]\nsources = [file(\"src/lib.cpp\")]\n\n[lib.lib.public]\ncxx_std = \"c++20\"\n",
    );
    p.write("lib/src/lib.cpp", "int lib_value() { return 42; }\n");

    p.run("app", &["build"]).success();
    assert_eq!(run_artifact(&build_dir(&p.path("app"), "debug").join("bin/app")), "v=42\n");
}

#[test]
fn an_explicit_std_flag_still_overrides_the_typed_property() {
    // 型付きのプロパティは語彙を閉じる。方言（`gnu++17`）は語彙の外なので、
    // 逃げ道として `cxx_flags` を残し、そちらが後に置かれて勝つ。
    let p = Project::new("cxx-std-escape");
    p.write("dowel.toml", "[package]\nname = \"e\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.e]\nsources = [file(\"src/main.cpp\")]\n\n\
         [bin.e.private]\ncxx_std   = \"c++17\"\ncxx_flags = [\"-std=gnu++17\"]\n",
    );
    p.write("src/main.cpp", "int main() { return 0; }\n");

    p.run(".", &["build"]).success();
    let compdb = std::fs::read_to_string(p.path("compile_commands.json")).unwrap();
    let entry = compdb_entry(&compdb, "main.cpp");
    let typed = entry.find("-std=c++17").expect("the typed standard is present");
    let escape = entry.find("-std=gnu++17").expect("the escape hatch is present");
    assert!(typed < escape, "the explicit flag must come last to win:\n{entry}");
}

#[test]
fn the_c_standard_reaches_c_sources_only() {
    let p = Project::new("c-std");
    p.write("dowel.toml", "[package]\nname = \"m\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.m]\nsources = [file(\"src/main.c\"), file(\"src/part.cpp\")]\n\n\
         [bin.m.private]\nc_std   = \"c11\"\ncxx_std = \"c++17\"\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\n\
         #if __STDC_VERSION__ < 201112L\n#error \"c_std did not reach the C compiler\"\n#endif\n\
         int part(void);\nint main(void) { printf(\"v=%d\\n\", part()); return 0; }\n",
    );
    p.write("src/part.cpp", "extern \"C\" int part(void) { return 42; }\n");

    p.run(".", &["build"]).success();
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/m")), "v=42\n");

    let compdb = std::fs::read_to_string(p.path("compile_commands.json")).unwrap();
    // 言語別に分かれる。C のコンパイルに `-std=c++17` が混ざれば C は通らない。
    let c = compdb_entry(&compdb, "main.c\"");
    assert!(c.contains("-std=c11"), "{c}");
    assert!(!c.contains("c++17"), "{c}");
    let cxx = compdb_entry(&compdb, "part.cpp");
    assert!(cxx.contains("-std=c++17"), "{cxx}");
    assert!(!cxx.contains("-std=c11"), "{cxx}");
}

/// `compile_commands.json` の1項目の本文。整形出力のため、項目は `{` で始まる。
fn compdb_entry(compdb: &str, file: &str) -> String {
    compdb
        .split('{')
        .find(|chunk| chunk.contains(file))
        .unwrap_or_else(|| panic!("no entry for {file}\n{compdb}"))
        .to_string()
}

/// 派生ファイルは、そのターゲットがどう到達されたかとは独立に作られる。
///
/// `lib` の `artifacts` が、そのライブラリに依存する `bin` を足した途端に
/// 黙って作られなくなる形（issue #64）。ninja からは派生が誰の入力にも
/// ならないため、`default` に並べない限り到達しない。
#[test]
fn a_library_keeps_producing_its_derived_file_when_a_binary_depends_on_it() {
    if !program_exists("objcopy") {
        eprintln!("skipping: objcopy is not on PATH");
        return;
    }
    let p = Project::new("artifacts-dependency");
    p.write("dowel.toml", "[package]\nname = \"fw\"\nversion = \"0\"\n");
    p.write("src/part.c", "int part(void) { return 42; }\n");
    p.write(
        "src/main.c",
        "#include <stdio.h>\nint part(void);\nint main(void) { printf(\"v=%d\\n\", part()); return 0; }\n",
    );
    let lib_only = "[lib.part]\nsources = [file(\"src/part.c\")]\n\n\
                    [lib.part.artifacts]\n\
                    stripped = { tool = \"objcopy\", args = [\"--strip-all\"] }\n";
    // A. ライブラリだけ。派生は作られる。
    p.write("dowel.build", lib_only);
    p.run(".", &["build"]).success();
    let dir = build_dir(&p.path("."), "debug");
    let derived = dir.join("lib/libpart.stripped");
    assert!(derived.exists(), "the derived file was not produced for a lone library");
    std::fs::remove_file(&derived).unwrap();

    // B. そのライブラリを使う bin を足す。`artifacts` の宣言は動かしていない。
    p.write(
        "dowel.build",
        &format!(
            "{lib_only}\n[bin.firmware]\nsources = [file(\"src/main.c\")]\n\n\
             [bin.firmware.private]\ndeps = [target(\"part\")]\n"
        ),
    );
    let r = p.run(".", &["build"]);
    r.success();
    assert!(dir.join("lib/libpart.a").exists(), "the archive is missing\n{r}");
    assert!(
        derived.exists(),
        "adding a dependent binary silently stopped the derived file from being produced\n{r}"
    );
    assert_eq!(run_artifact(&dir.join("bin/firmware")), "v=42\n");

    // 実行器を跨いで同じものが出来ること。派生は誰の入力にもならないため、
    // ninja の `default` から漏れると direct とだけ食い違う（issue #41 と同じ形）。
    std::fs::remove_file(&derived).unwrap();
    p.run(".", &["build", "--backend=direct"]).success();
    assert!(derived.exists(), "the direct backend and ninja disagree about the derived file");
}

/// `[<kind>.<name>.inspect]` — 成果物について報告する検査（issue #60）。
///
/// 変換と違い出力を持たないため、`build` の既定には入らず `dowel inspect`
/// が走らせる。宣言した道具の出力がそのまま届くこと、道具はトリプルごとに
/// 選ばれること、失敗が失敗として返ることを見る。
#[test]
fn declared_inspections_run_and_report_through_dowel_inspect() {
    if !program_exists("size") || !program_exists("nm") {
        eprintln!("skipping: binutils are not on PATH");
        return;
    }
    let p = Project::new("inspect");
    p.write("dowel.toml", "[package]\nname = \"fw\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.firmware]\nsources = glob(\"src/*.c\")\n\n\
         [bin.firmware.inspect]\n\
         sections = { tool = \"size\", args = [\"-A\"] }\n\
         symbols  = { tool = \"nm\", args = [\"--size-sort\"] }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    // 検査は成果物を要する。`inspect` は先に組む。
    let r = p.run(".", &["inspect"]);
    r.success();
    // 道具の出力はそのまま stdout へ通す。dowel は解釈しない。
    r.stdout_contains(".text");
    r.stdout_contains("main");
    // どの検査の出力かは stderr の見出しで分かる。
    r.stderr_contains("sections");
    r.stderr_contains("symbols");

    // 機械可読の形。1検査1行。
    let j = p.run(".", &["inspect", "--message-format=json"]);
    j.success();
    j.stdout_contains("\"inspection\":\"sections\"");
    j.stdout_contains("\"tool\":\"size\"");
    j.stdout_contains("\"ok\":true");
    assert_eq!(j.stdout.lines().count(), 2, "expected one line per inspection\n{j}");

    // 検査は成果物を作らない。`build` の既定にも増分にも入らない。
    let b = p.run(".", &["build"]);
    b.success();
    assert!(!b.stderr.contains("sections"), "an inspection ran during build\n{b}");
}

#[test]
fn the_inspection_tool_is_selected_by_the_toolchain_declaration() {
    let p = Project::new("inspect-tool-selection");
    let wrapper = p.path("fake-size");
    std::fs::write(&wrapper, "#!/bin/sh\necho \"flash budget: 1024\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"fw\"\nversion = \"0\"\n\n[toolchain]\nsize = \"{}\"\n",
            wrapper.display()
        ),
    );
    p.write(
        "dowel.build",
        "[bin.firmware]\nsources = glob(\"src/*.c\")\n\n\
         [bin.firmware.inspect]\nbudget = { tool = \"size\" }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["inspect"]);
    r.success();
    r.stdout_contains("flash budget: 1024");
}

#[test]
fn a_failing_inspection_fails_the_run() {
    // 検査は報告であって、報告が失敗したら失敗である。`size` を判定に
    // 使う形（予算）を将来足すときの土台でもある。
    let p = Project::new("inspect-failure");
    let wrapper = p.path("failing-size");
    std::fs::write(&wrapper, "#!/bin/sh\necho 'over budget' >&2\nexit 3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"fw\"\nversion = \"0\"\n\n[toolchain]\nsize = \"{}\"\n",
            wrapper.display()
        ),
    );
    p.write(
        "dowel.build",
        "[bin.firmware]\nsources = glob(\"src/*.c\")\n\n\
         [bin.firmware.inspect]\nbudget = { tool = \"size\" }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["inspect"]);
    r.failure();
    r.stderr_contains("over budget");
    r.stderr_contains("exit code 3");
}

#[test]
fn a_project_without_inspections_says_so_instead_of_failing() {
    let p = Project::new("inspect-none");
    p.write("dowel.toml", "[package]\nname = \"n\"\nversion = \"0\"\n");
    p.write("dowel.build", "[bin.n]\nsources = glob(\"src/*.c\")\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["inspect"]);
    r.success();
    r.stderr_contains("no inspections");
}

/// `/` を含む機能名が、ビルドディレクトリを2階層に割らないこと（issue #68）。
///
/// この形は `dep/feature` を書いたときに現れる。**その名前が依存先の機能を
/// 有効にするかどうかは、この検査の対象ではない**——機能の転送は実装されて
/// おらず（`resolve_features` は根の `[features]` を閉包するだけで、名前を
/// 依存先の名前空間へ翻訳しない）、docs も約束していない。ここで見るのは
/// 「構成が1ディレクトリである」ことだけである。
#[test]
fn a_forwarded_feature_does_not_split_the_build_directory() {
    let p = Project::new("forwarded-feature");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [features]\ndefault = []\nx = [\"lib/y\"]\n\n\
         [[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n[bin.app.private]\ndeps = [dep(\"lib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\nint limit(void);\nint main(void) { printf(\"n=%d\\n\", limit()); return 0; }\n",
    );
    p.write(
        "lib/dowel.toml",
        "[package]\nname = \"lib\"\nversion = \"0\"\n\n[features]\ndefault = []\ny = []\n",
    );
    p.write(
        "lib/dowel.build",
        "[lib.lib]\nsources = [file(\"src/lib.c\")]\n\n\
         [lib.lib.public]\ndefines = { LIMIT = 4096 } when feature.y\n",
    );
    p.write(
        "lib/src/lib.c",
        "#ifndef LIMIT\n#define LIMIT 256\n#endif\nint limit(void) { return LIMIT; }\n",
    );

    let r = p.run("app", &["build", "--features=x"]);
    r.success();

    // `.dowel/build` の直下に構成が1つだけ並ぶ。`/` が区切りとして
    // 展開されていれば、名前が切れた親と、その下の階層に割れる。
    let roots: Vec<String> = std::fs::read_dir(p.path("app/.dowel/build"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(roots.len(), 1, "the configuration is not one directory: {roots:?}");
    let name = &roots[0];
    assert!(!name.contains('/'), "{name}");
    assert!(name.contains("lib--y"), "the forwarded name was not folded: {name}");

    // 成果物はその1階層の下にある。
    let dir = p.path("app/.dowel/build").join(name);
    assert!(dir.join("bin/app").exists(), "the artifact is not under the configuration directory");
    run_artifact(&dir.join("bin/app"));
}

/// 狭い呼び出しの後の広い呼び出しが、編集も無いのに組み直さないこと
/// （issue #69）。記録は併合されなければならない。
#[test]
fn a_narrow_invocation_does_not_make_the_next_full_build_redo_work() {
    let p = Project::new("record-merge");
    p.write("dowel.toml", "[package]\nname = \"w\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.one]\nsources = [file(\"src/one.c\")]\n\n[bin.two]\nsources = [file(\"src/two.c\")]\n",
    );
    p.write("src/one.c", "int main(void) { return 0; }\n");
    p.write("src/two.c", "int main(void) { return 0; }\n");

    let ran = |r: &common::Run| -> usize {
        // `ran N actions` を読む。debug 記録に出る。
        r.stderr
            .lines()
            .find_map(|l| l.split("ran ").nth(1).and_then(|s| s.split(' ').next()))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no action count in the log\n{r}"))
    };
    let build = |args: &[&str]| {
        let mut v = vec!["build", "--backend=direct", "--log-level=debug"];
        v.extend_from_slice(args);
        p.run(".", &v)
    };

    build(&[]).success();
    let second = build(&[]);
    second.success();
    assert_eq!(ran(&second), 0, "a repeated build was not a no-op\n{second}");

    // 片方だけを名指しする。記録から他方が落ちてはならない。
    build(&["one"]).success();

    let after = build(&[]);
    after.success();
    assert_eq!(
        ran(&after),
        0,
        "the narrow invocation dropped records and the full build redid work\n{after}"
    );
}

/// `link_flags` の `Path` 要素が絶対パスへ展開されること（issue #70）。
///
/// ベアメタルではリンカスクリプトを省略できず、それはパッケージの中に置く。
/// リンクの作業ディレクトリはビルドディレクトリなので、相対の文字列では届かない。
#[test]
fn a_path_in_link_flags_expands_to_its_absolute_path() {
    let p = Project::new("link-script");
    p.write("dowel.toml", "[package]\nname = \"fw\"\nversion = \"0\"\n");
    // 報告と同じ形。freestanding なので、スクリプトが配置を全て決める。
    p.write(
        "dowel.build",
        "[bin.fw]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.fw.private]\nlink_flags = [\"-nostdlib\", \"-T\", file(\"ld/app.ld\")]\n",
    );
    p.write("src/main.c", "void _reset(void) { for (;;) {} }\n");
    p.write(
        "ld/app.ld",
        "ENTRY(_reset)\nSECTIONS {\n  . = 0x8000000;\n  .text : { *(.text*) }\n\
         .data : { *(.data*) }\n  .bss : { *(.bss*) }\n}\n",
    );

    // 道が届かなければ `cannot open linker script file` で落ちる。
    let r = p.run(".", &["build"]);
    r.success();

    // 渡った引数が絶対パスであること。
    let g = p.run(".", &["graph", "--kind=action", "--format=json"]);
    g.success();
    // 生成パスは `../..` を含むため、模型側の正規化に合わせて比べる。
    let script = std::fs::canonicalize(p.path("ld/app.ld")).unwrap().display().to_string();
    assert!(g.stdout.contains(&script), "the script was not passed as an absolute path\n{g}");

    // スクリプトが実際に効いていること。配置は既定ではなく宣言どおりになる。
    if program_exists("readelf") {
        let bin = build_dir(&p.path("."), "debug").join("bin/fw");
        let out = std::process::Command::new("readelf").args(["-l"]).arg(&bin).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("0x0000000008000000"), "the script did not place the image:\n{text}");
    }
}

#[test]
fn a_string_in_link_flags_is_still_passed_through() {
    // 道を受けるようにしても、道を含まないフラグはこれまでどおり書ける。
    let p = Project::new("link-flags-str");
    p.write("dowel.toml", "[package]\nname = \"m\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.m]\nsources = [file(\"src/main.c\")]\n\n[bin.m.private]\nlink_flags = [\"-lm\"]\n",
    );
    p.write(
        "src/main.c",
        "#include <math.h>\n#include <stdio.h>\n\
         int main(void) { printf(\"v=%d\\n\", (int) sqrt(1764.0)); return 0; }\n",
    );
    p.run(".", &["build"]).success();
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/m")), "v=42\n");
}

/// パッケージが対象とするトリプルを宣言できること（issue #71）。
#[test]
fn a_package_can_say_which_targets_it_is_for() {
    let p = Project::new("package-targets");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"blink\"\nversion = \"0.1.0\"\ntargets = [\"aarch64-unknown-linux-gnu\"]\n\n\
         [toolchain.aarch64-unknown-linux-gnu]\nc = \"aarch64-linux-gnu-gcc\"\n",
    );
    p.write("dowel.build", "[bin.firmware]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    // `--target` の付け忘れが、ホスト向けの成功として返ってはならない。
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unsupported-target");
    r.stderr_contains("is not built for");
    r.stderr_contains("pass --target=aarch64-unknown-linux-gnu");
    // 何も組まれていない。
    assert!(!p.path(".dowel/build").exists(), "artifacts were produced for a refused target");
}

#[test]
fn a_package_without_a_target_declaration_still_builds_anywhere() {
    // 宣言が無ければ従来どおり。クロスのときだけ道具を替えたい木は、
    // `[toolchain.<triple>]` を持ちつつ対象を絞らない（issue #71）。
    let p = Project::new("package-targets-absent");
    p.write(
        "dowel.toml",
        "[package]\nname = \"httpd\"\nversion = \"0\"\n\n\
         [toolchain.aarch64-unknown-linux-gnu]\nc = \"aarch64-linux-gnu-gcc\"\n",
    );
    p.write("dowel.build", "[bin.httpd]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    p.run(".", &["build"]).success();
}

/// 機能はそれを宣言したパッケージのものであり、`dep/feat` は依存の機能を
/// 有効にする（ADR-0017）。
#[test]
fn a_feature_forwarded_to_a_dependency_reaches_it() {
    let p = Project::new("feature-forward");
    // 親の機能名 `x` と、依存の機能名 `y` をわざと違えてある。名前が同じだと
    // 平坦な集合でも「効いて見える」ため、転送そのものを見られない。
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [features]\ndefault = []\nx = [\"lib/y\"]\n\n\
         [[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n[bin.app.private]\ndeps = [dep(\"lib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\nint limit(void);\nint main(void) { printf(\"n=%d\\n\", limit()); return 0; }\n",
    );
    p.write(
        "lib/dowel.toml",
        "[package]\nname = \"lib\"\nversion = \"0\"\n\n[features]\ndefault = []\ny = []\n",
    );
    p.write(
        "lib/dowel.build",
        "[lib.lib]\nsources = [file(\"src/lib.c\")]\n\n\
         [lib.lib.public]\ndefines = { LIMIT = 4096 } when feature.y\n",
    );
    p.write(
        "lib/src/lib.c",
        "#ifndef LIMIT\n#define LIMIT 256\n#endif\nint limit(void) { return LIMIT; }\n",
    );

    // 転送しなければ依存の既定のまま。
    p.run("app", &["build"]).success();
    let plain = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&plain), "n=256\n");

    // 転送すれば依存に届く。
    let r = p.run("app", &["build", "--features=x"]);
    r.success();
    let dir = std::fs::read_dir(p.path("app/.dowel/build"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|d| d.file_name().unwrap().to_string_lossy().contains("lib--y"))
        .expect("the forwarded feature is part of the configuration identifier");
    assert_eq!(run_artifact(&dir.join("bin/app")), "n=4096\n");
}

#[test]
fn a_feature_of_one_package_does_not_answer_for_another() {
    // 同じ名前の機能を両方が宣言する。親で有効にしても、依存の同名の機能は
    // 有効にならない——機能は宣言したパッケージのものである（ADR-0017）。
    let p = Project::new("feature-scope");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [features]\ndefault = []\nfast = []\n\n\
         [[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n[bin.app.private]\ndeps = [dep(\"lib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\nint limit(void);\nint main(void) { printf(\"n=%d\\n\", limit()); return 0; }\n",
    );
    p.write(
        "lib/dowel.toml",
        "[package]\nname = \"lib\"\nversion = \"0\"\n\n[features]\ndefault = []\nfast = []\n",
    );
    p.write(
        "lib/dowel.build",
        "[lib.lib]\nsources = [file(\"src/lib.c\")]\n\n\
         [lib.lib.public]\ndefines = { LIMIT = 4096 } when feature.fast\n",
    );
    p.write(
        "lib/src/lib.c",
        "#ifndef LIMIT\n#define LIMIT 256\n#endif\nint limit(void) { return LIMIT; }\n",
    );

    p.run("app", &["build", "--features=fast"]).success();
    let dir = std::fs::read_dir(p.path("app/.dowel/build"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|d| d.file_name().unwrap().to_string_lossy().contains("app--fast"))
        .expect("the feature is qualified by its package");
    assert_eq!(
        run_artifact(&dir.join("bin/app")),
        "n=256\n",
        "the root's `fast` leaked into the dependency's identically named feature"
    );
}

#[test]
fn a_forward_to_an_undeclared_dependency_is_refused() {
    let p = Project::new("feature-forward-unknown-dep");
    p.write(
        "dowel.toml",
        "[package]\nname = \"a\"\nversion = \"0\"\n\n[features]\ndefault = []\nx = [\"ghost/y\"]\n",
    );
    p.write("dowel.build", "[bin.a]\nsources = glob(\"src/*.c\")\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check", "--features=x"]);
    r.failure();
    r.stderr_contains("undeclared-dependency");
    r.stderr_contains("ghost");
}

#[test]
fn a_forward_naming_a_feature_the_dependency_does_not_declare_is_refused() {
    // 宣言されていない名前は依存の側で偽と評価されるだけで何も起きない。
    // 綴りを誤った転送と、意図して無効にした機能の区別が付かなくなる。
    let p = Project::new("feature-forward-typo");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [features]\ndefault = []\nx = [\"lib/yy\"]\n\n\
         [[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    );
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    p.write(
        "lib/dowel.toml",
        "[package]\nname = \"lib\"\nversion = \"0\"\n\n[features]\ndefault = []\ny = []\n",
    );
    p.write("lib/dowel.build", "[lib.lib]\nsources = glob(\"src/*.c\")\n");
    p.write("lib/src/lib.c", "int f(void) { return 0; }\n");

    let r = p.run("app", &["check", "--features=x"]);
    r.failure();
    r.stderr_contains("unknown-feature");
    r.stderr_contains("did you mean `y`?");
}

/// `dowel.toml` に書いた `[runner.<triple>]` が黙って無視されないこと
/// （issue #74）。診断が「宣言が無い」と言う一方で宣言は書かれている、
/// という食い違いを断つ。
#[test]
fn a_runner_written_into_dowel_toml_is_not_silently_ignored() {
    let p = Project::new("runner-misplaced");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"r\"\nversion = \"0.0.0\"\n\n\
         [runner.thumbv7em-none-eabihf]\ncommand = \"qemu-system-arm\"\n",
    );
    p.write("dowel.build", "[bin.r]\nsources = glob(\"src/*.c\")\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-table");
    r.stderr_contains("runner");
    // どこへ書くかを述べる。述べなければ、利用者は書いたものを見ながら
    // 何が悪いのか分からない。
    r.stderr_contains("declared in `dowel.build`");
}

#[test]
fn an_unknown_table_in_dowel_toml_gets_a_suggestion() {
    let p = Project::new("manifest-typo");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"m\"\nversion = \"0\"\n\n[feature]\ndefault = []\n",
    );
    p.write("dowel.build", "[bin.m]\nsources = glob(\"src/*.c\")\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-table");
    r.stderr_contains("did you mean `features`?");
}

#[test]
fn a_reserved_table_in_dowel_toml_is_still_accepted() {
    // `[policy]` は「予約済みで、まだ読まない」と文書に書いてある。
    // 書いてあるものを拒むと、文書と実装が食い違う。
    let p = Project::new("manifest-reserved");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"m\"\nversion = \"0\"\n\n[policy]\naudit = true\n",
    );
    p.write("dowel.build", "[bin.m]\nsources = glob(\"src/*.c\")\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    p.run(".", &["check"]).success();
}

/// 依存が名乗る出所は1つ（issue #79）。
///
/// 0個は `incomplete-dependency` で拒んでいた。2個は黙って受け、しかも
/// `path` が勝っていた。切り替えの途中で消し忘れると、その木を持たない
/// 誰かが組むまで気づかない。
fn two_source_project(name: &str, extra: &str) -> Project {
    let p = Project::new(name);
    p.write("libfoo/dowel.toml", "[package]\nname    = \"libfoo\"\nversion = \"0.1.0\"\n");
    p.write("libfoo/dowel.build", "[lib.foo]\nsources = glob(\"src/*.c\")\n");
    p.write("libfoo/src/foo.c", "int foo(void) { return 1; }\n");
    p.write(
        "app/dowel.toml",
        &format!(
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n\
             [[dependencies]]\nname = \"libfoo\"\npath = \"../libfoo\"\n{extra}"
        ),
    );
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    p
}

#[test]
fn a_dependency_that_names_two_sources_is_refused() {
    // 取りに行けない git を書いても、以前は `path` が勝って黙って通っていた。
    let p = two_source_project(
        "dep-two-sources",
        "git  = \"https://example.invalid/libfoo\"\n\
         rev  = \"0123456789012345678901234567890123456789\"\n",
    );
    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("conflicting-dependency-source");
    // 両方の宣言が見えること。どちらを消すのかは利用者が決める。
    r.stderr_contains("a local path");
    r.stderr_contains("and a git repository");
    r.stderr_contains("exactly one source");
}

#[test]
fn the_same_holds_when_the_two_sources_are_a_path_and_a_version() {
    let p = two_source_project("dep-path-and-version", "version = \"9.0\"\n");
    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("conflicting-dependency-source");
    r.stderr_contains("a system package");
}

#[test]
fn one_source_is_still_accepted() {
    // 規則は「ちょうど1つ」であって「path を疑う」ではない。
    let p = two_source_project("dep-one-source", "");
    p.run("app", &["check"]).success();
}

/// 配ることを前提にした C のライブラリを、C++ の利用者が自分の札のまま使える
/// （issue #78、ADR-0019）。
///
/// ライブラリの作者は利用者の言語を知らない。言語の札を1つ選ぶと、それを
/// 全ての利用者に強制することになる。`abi = "c"` は境界を指す札であり、
/// `extern "C"` の面しか持たない公開面が名乗る。
fn c_library_project(name: &str, lib_abi: &str, consumer_abi: &str) -> Project {
    let p = Project::new(name);
    p.write("libhash/dowel.toml", "[package]\nname    = \"libhash\"\nversion = \"0.4.0\"\n");
    p.write(
        "libhash/dowel.build",
        &format!(
            r#"
[lib.hash]
sources = glob("src/*.c")

[lib.hash.public]
includes = [dir("include")]
abi      = "{lib_abi}"
"#
        ),
    );
    p.write(
        "libhash/include/hash.h",
        "#pragma once\n#ifdef __cplusplus\nextern \"C\" {\n#endif\nint hash_of(const char *s);\n#ifdef __cplusplus\n}\n#endif\n",
    );
    p.write(
        "libhash/src/hash.c",
        "#include \"hash.h\"\nint hash_of(const char *s) { int h = 0; while (*s) h = h * 31 + *s++; return h; }\n",
    );
    p.write(
        "cxxtool/dowel.toml",
        "[package]\nname    = \"cxxtool\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"libhash\"\npath = \"../libhash\"\n",
    );
    p.write(
        "cxxtool/dowel.build",
        &format!(
            r#"
[bin.hashcxx]
sources = glob("src/*.cpp")

[bin.hashcxx.private]
deps = [dep("libhash")]
abi  = "{consumer_abi}"
"#
        ),
    );
    p.write(
        "cxxtool/src/main.cpp",
        r#"#include <cstdio>
#include <string>
#include "hash.h"
int main() {
    std::string s = "abc";
    std::printf("h=%d\n", hash_of(s.c_str()));
    return 0;
}
"#,
    );
    p
}

#[test]
fn a_cxx_consumer_can_declare_its_own_abi_label_and_still_use_a_c_library() {
    if !program_exists("c++") {
        return;
    }
    let p = c_library_project("abi-c-boundary", "c", "gnu++17");
    p.run("cxxtool", &["build"]).success();
    let bin = build_dir(&p.path("cxxtool"), "debug").join("bin/hashcxx");
    assert_eq!(run_artifact(&bin), "h=96354\n");
}

#[test]
fn a_language_label_on_the_library_still_forces_the_consumer() {
    // 変えたのは札の語彙であって、突き合わせそのものではない。言語の札同士は
    // 依然として一致を要する。
    let p = c_library_project("abi-c-still-checked", "gnu11", "gnu++17");
    let r = p.run("cxxtool", &["build"]);
    r.failure();
    r.stderr_contains("abi-mismatch");
}

#[test]
fn the_c_label_does_not_hide_a_real_mismatch_behind_it() {
    // `c` は制約を足さないだけで、消しはしない。`c` の面の向こうから届いた
    // 本物の札は、利用者の札と突き合わされる。
    let p = c_library_project("abi-c-transparent", "c", "gnu++17");
    p.write("libhash/dowel.toml", "[package]\nname    = \"libhash\"\nversion = \"0.4.0\"\n\n[[dependencies]]\nname = \"libcore\"\npath = \"../libcore\"\n");
    p.write(
        "libhash/dowel.build",
        r#"
[lib.hash]
sources = glob("src/*.c")

[lib.hash.public]
includes = [dir("include")]
abi      = "c"
deps     = [dep("libcore")]
"#,
    );
    p.write("libcore/dowel.toml", "[package]\nname    = \"libcore\"\nversion = \"0.1.0\"\n");
    p.write(
        "libcore/dowel.build",
        "[lib.core]\nsources = glob(\"src/*.c\")\n\n[lib.core.public]\nabi = \"gnu11\"\n",
    );
    p.write("libcore/src/core.c", "int core(void) { return 7; }\n");

    let r = p.run("cxxtool", &["build"]);
    r.failure();
    r.stderr_contains("abi-mismatch");
}

/// パッケージの定数（issue #80、ADR-0020）。
///
/// ライブラリの版は `dowel.toml` と公開する見出しの2か所にあり、一致は誰も
/// 見ていなかった。`pkg.version` で1か所に戻す。
fn versioned_project(name: &str, version: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", &format!("[package]\nname    = \"hashx\"\nversion = \"{version}\"\n"));
    p.write(
        "dowel.build",
        r#"
[bin.hashsum]
sources = glob("src/*.c")

[bin.hashsum.private]
defines = { HASHX_VERSION = pkg.version, HASHX_NAME = pkg.name }
"#,
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\nint main(void) { printf(\"%s %s\\n\", HASHX_NAME, HASHX_VERSION); return 0; }\n",
    );
    p
}

#[test]
fn the_manifest_version_reaches_the_translation() {
    let p = versioned_project("pkg-version", "0.4.0");
    p.run(".", &["build"]).success();
    let bin = build_dir(&p.path("."), "debug").join("bin/hashsum");
    assert_eq!(run_artifact(&bin), "hashx 0.4.0\n");
}

#[test]
fn moving_the_manifest_version_alone_is_noticed() {
    // 版だけを動かす。ソースは1文字も変わらない。以前は無診断で通り、
    // 成果物は古い版を答え続けていた。
    let p = versioned_project("pkg-version-moved", "0.4.0");
    p.run(".", &["build"]).success();
    let bin = build_dir(&p.path("."), "debug").join("bin/hashsum");
    assert_eq!(run_artifact(&bin), "hashx 0.4.0\n");

    p.write("dowel.toml", "[package]\nname    = \"hashx\"\nversion = \"9.9.9\"\n");
    p.run(".", &["build"]).success();
    assert_eq!(run_artifact(&bin), "hashx 9.9.9\n", "the manifest version did not reach the build");
}

#[test]
fn a_dependency_reads_its_own_version_not_the_root_package_s() {
    // 定数はその値を宣言したパッケージのものである。機能と同じ扱い（ADR-0017）。
    let p = Project::new("pkg-version-per-package");
    p.write("liblog/dowel.toml", "[package]\nname    = \"liblog\"\nversion = \"2.1.0\"\n");
    p.write(
        "liblog/dowel.build",
        r#"
[lib.log]
sources = glob("src/*.c")

[lib.log.public]
includes = [dir("include")]

[lib.log.private]
defines = { LOG_VERSION = pkg.version }
"#,
    );
    p.write("liblog/include/log.h", "#pragma once\nconst char *log_version(void);\n");
    p.write(
        "liblog/src/log.c",
        "#include \"log.h\"\nconst char *log_version(void) { return LOG_VERSION; }\n",
    );
    p.write(
        "app/dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"liblog\"\npath = \"../liblog\"\n",
    );
    p.write(
        "app/dowel.build",
        r#"
[bin.app]
sources = glob("src/*.c")

[bin.app.private]
deps    = [dep("liblog")]
defines = { APP_VERSION = pkg.version }
"#,
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"log.h\"\nint main(void) { printf(\"app=%s log=%s\\n\", APP_VERSION, log_version()); return 0; }\n",
    );

    p.run("app", &["build"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "app=0.1.0 log=2.1.0\n");
}

#[test]
fn an_unknown_package_constant_gets_a_suggestion() {
    let p = versioned_project("pkg-typo", "0.1.0");
    p.write(
        "dowel.build",
        "[bin.hashsum]\nsources = glob(\"src/*.c\")\n\n[bin.hashsum.private]\ndefines = { V = pkg.versoin }\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-pkg-constant");
    r.stderr_contains("did you mean `version`?");
}

#[test]
fn a_package_constant_is_not_a_configuration_key() {
    // 版はビルドが分岐する軸ではない。受けると、そう述べることになる。
    let p = versioned_project("pkg-not-cfg", "0.1.0");
    p.write(
        "dowel.build",
        r#"
[bin.hashsum]
sources = glob("src/*.c")

[bin.hashsum.private]
defines = match pkg.version {
    _ => { V = 1 },
}
"#,
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("not-a-configuration-key");
    r.stderr_contains("value position");
}

#[test]
fn a_configuration_reference_still_cannot_appear_in_a_value_position() {
    // 値の位置に書けるようになったのは `pkg.*` だけである。
    let p = versioned_project("pkg-only-value-position", "0.1.0");
    p.write(
        "dowel.build",
        "[bin.hashsum]\nsources = glob(\"src/*.c\")\n\n[bin.hashsum.private]\ndefines = { V = cfg.opt }\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unexpected-reference");
}

/// 排他な機能（issue #82、ADR-0021）。
///
/// 機能は加算である。`--features=x11` は `default` を落とさない。実装の択一を
/// 条件付きの `sources` で書くとこれと真正面からぶつかり、`bin` ならリンカの
/// `multiple definition`、`lib` なら**組み上がって片方が黙って勝つ**。
fn two_backend_project(name: &str, kind: &str, exclusive: &str) -> Project {
    let p = Project::new(name);
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"shell\"\nversion = \"0.1.0\"\n\n\
             [features]\ndefault  = [\"headless\"]\nheadless = []\nx11      = []\n{exclusive}"
        ),
    );
    p.write(
        "dowel.build",
        &format!(
            r#"
[{kind}.shell]
sources = [
    file("src/main.c"),
    file("src/shell_x11.c")      when feature.x11,
    file("src/shell_headless.c") when feature.headless,
]
"#
        ),
    );
    p.write("src/main.c", "const char *shell_name(void);\nint main(void) { return 0; }\n");
    p.write("src/shell_x11.c", "const char *shell_name(void) { return \"x11\"; }\n");
    p.write("src/shell_headless.c", "const char *shell_name(void) { return \"headless\"; }\n");
    p
}

#[test]
fn two_exclusive_features_enabled_at_once_are_refused() {
    // `--features=x11` は `default = ["headless"]` を落とさない。宣言してあれば
    // リンカに渡す前に断る。
    let p =
        two_backend_project("features-exclusive", "bin", "exclusive = [[\"headless\", \"x11\"]]\n");
    let r = p.run(".", &["check", "--features=x11"]);
    r.failure();
    r.stderr_contains("conflicting-features");
    r.stderr_contains("headless");
    r.stderr_contains("x11");
    // 忘れやすいのは `default` の側である。名指しで述べる。
    r.stderr_contains("comes from `default`");
    r.stderr_contains("--no-default-features");
}

#[test]
fn dropping_the_defaults_makes_the_same_manifest_build() {
    // 断るのは組み合わせであって、機能そのものではない。
    let p = two_backend_project(
        "features-exclusive-ok",
        "bin",
        "exclusive = [[\"headless\", \"x11\"]]\n",
    );
    p.run(".", &["build", "--no-default-features", "--features=x11"]).success();
    p.run(".", &["build"]).success();
}

#[test]
fn a_library_with_two_implementations_says_so_instead_of_keeping_one() {
    // ここが危ない側である。宣言が無ければ組み上がり、書庫はリンカが最初に
    // 到達した部材だけを残す。どちらが入ったかはマニフェストから読めない。
    let p = two_backend_project(
        "features-exclusive-lib",
        "lib",
        "exclusive = [[\"headless\", \"x11\"]]\n",
    );
    let r = p.run(".", &["build", "--features=x11"]);
    r.failure();
    r.stderr_contains("conflicting-features");
}

#[test]
fn without_the_declaration_nothing_changes() {
    // 宣言は任意である。書かない木は今までどおりに振る舞う——`lib` は
    // 黙って組み上がる。これが宣言を要する理由でもある。
    let p = two_backend_project("features-exclusive-absent", "lib", "");
    p.run(".", &["build", "--features=x11"]).success();
}

#[test]
fn an_exclusive_group_naming_an_undeclared_feature_is_refused() {
    let p = two_backend_project(
        "features-exclusive-typo",
        "bin",
        "exclusive = [[\"headless\", \"x11l\"]]\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-feature");
    r.stderr_contains("did you mean `x11`?");
}

#[test]
fn an_exclusive_group_of_one_forbids_nothing_and_says_so() {
    let p = two_backend_project("features-exclusive-single", "bin", "exclusive = [[\"x11\"]]\n");
    let r = p.run(".", &["check"]);
    r.success();
    r.stderr_contains("empty-exclusive-group");
}

#[test]
fn a_match_selects_exactly_one_implementation() {
    // 正しい書き方。排他の宣言が無くても1つしか選ばれない。
    let p = Project::new("features-match");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"shell\"\nversion = \"0.1.0\"\n\n\
         [features]\ndefault = []\nx11     = []\n",
    );
    p.write(
        "dowel.build",
        r#"
[bin.shell]
sources = [
    file("src/main.c"),
    match feature.x11 {
        true  => file("src/shell_x11.c"),
        false => file("src/shell_headless.c"),
    },
]
"#,
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\nconst char *shell_name(void);\nint main(void) { printf(\"%s\\n\", shell_name()); return 0; }\n",
    );
    p.write("src/shell_x11.c", "const char *shell_name(void) { return \"x11\"; }\n");
    p.write("src/shell_headless.c", "const char *shell_name(void) { return \"headless\"; }\n");

    p.run(".", &["build"]).success();
    let bin = build_dir(&p.path("."), "debug").join("bin/shell");
    assert_eq!(run_artifact(&bin), "headless\n");

    p.run(".", &["build", "--features=x11"]).success();
    let bin = build_dir(&p.path("."), "debug-shell--x11").join("bin/shell");
    assert_eq!(run_artifact(&bin), "x11\n");
}

/// `[test.<name>.cases]` — 1本の実行ファイルから複数のテストを登録する。
///
/// ctest の `add_test` に相当する。事例を分けるのは引数であり、翻訳の単位は
/// 増えない。
fn case_project(name: &str, cases: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"suite\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        &format!("[test.suite]\nsources = glob(\"tests/*.c\")\n\n[test.suite.cases]\n{cases}"),
    );
    // 第1引数で振る舞いを変える。`fail` は非零、`hang` は終わらない。
    p.write(
        "tests/suite.c",
        r#"#include <stdio.h>
#include <string.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    const char *what = argc > 1 ? argv[1] : "ok";
    if (strcmp(what, "fail") == 0) { return 3; }
    if (strcmp(what, "hang") == 0) { for (;;) { } }
    if (strcmp(what, "env") == 0) {
        const char *v = getenv("SUITE_MODE");
        printf("mode=%s\n", v ? v : "(unset)");
        return v && strcmp(v, "strict") == 0 ? 0 : 1;
    }
    printf("ran %s\n", what);
    return 0;
}
"#,
    );
    p
}

#[test]
fn one_binary_registers_several_tests() {
    let p = case_project(
        "cases-basic",
        "parse = { args = [\"parse\"] }\nemit  = { args = [\"emit\"] }\n",
    );
    let r = p.run(".", &["test"]);
    r.success();
    // 事例ごとに1行。ラベルは `<ターゲット>/<事例>`。
    r.stderr_contains("suite:suite/parse ... ok");
    r.stderr_contains("suite:suite/emit ... ok");
    r.stderr_contains("running 2 tests");
}

#[test]
fn a_target_without_cases_is_still_one_test() {
    // 宣言しない木は今までどおり。
    let p = case_project("cases-absent", "");
    p.write("dowel.build", "[test.suite]\nsources = glob(\"tests/*.c\")\n");
    let r = p.run(".", &["test"]);
    r.success();
    r.stderr_contains("suite:suite ... ok");
    r.stderr_contains("running 1 test");
}

#[test]
fn a_case_can_expect_a_nonzero_exit() {
    let p =
        case_project("cases-should-fail", "rejects = { args = [\"fail\"], should_fail = true }\n");
    p.run(".", &["test"]).success().stderr_contains("suite:suite/rejects ... ok");
}

#[test]
fn a_should_fail_case_that_succeeds_is_a_failure_and_says_why() {
    // 「状態0で終了した」だけでは、なぜ失敗なのか読めない。期待の側を述べる。
    let p = case_project(
        "cases-should-fail-passed",
        "rejects = { args = [\"ok\"], should_fail = true }\n",
    );
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("`should_fail` expects a nonzero exit");
}

#[test]
fn a_case_that_does_not_finish_is_killed_and_reported() {
    let p = case_project("cases-timeout", "slow = { args = [\"hang\"], timeout = 1 }\n");
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("suite:suite/slow ... FAILED");
    r.stderr_contains("timed out");
}

#[test]
fn a_case_can_set_its_own_environment() {
    let p = case_project(
        "cases-env",
        "strict = { args = [\"env\"], env = { SUITE_MODE = \"strict\" } }\n",
    );
    p.run(".", &["test"]).success().stderr_contains("suite:suite/strict ... ok");
}

#[test]
fn labels_select_which_cases_run() {
    let p = case_project(
        "cases-labels",
        "quick = { args = [\"quick\"], labels = [\"fast\"] }\n\
         heavy = { args = [\"heavy\"], labels = [\"slow\"] }\n",
    );
    let r = p.run(".", &["test", "--label=fast"]);
    r.success();
    r.stderr_contains("running 1 test");
    r.stderr_contains("suite:suite/quick");
    assert!(!r.stderr.contains("suite:suite/heavy"), "the slow case ran anyway\n{r}");

    // 誰も名乗っていない名前は、黙って0件成功にせず理由を述べる。
    let r = p.run(".", &["test", "--label=nosuch"]);
    r.success();
    r.stderr_contains("no test carries `nosuch`");
}

#[test]
fn rerunning_only_the_failures_works_at_case_granularity() {
    let p = case_project(
        "cases-failed",
        "good = { args = [\"good\"] }\nbad  = { args = [\"fail\"] }\n",
    );
    p.run(".", &["test"]).failure();
    let r = p.run(".", &["test", "--failed"]);
    r.failure();
    r.stderr_contains("running 1 test");
    r.stderr_contains("suite:suite/bad");
    assert!(!r.stderr.contains("suite:suite/good"), "a passing case was rerun\n{r}");
}

#[test]
fn a_case_result_is_machine_readable() {
    let p = case_project("cases-json", "slow = { args = [\"hang\"], timeout = 1 }\n");
    let r = p.run(".", &["test", "--message-format=json"]);
    r.failure();
    r.stdout_contains("\"target\":\"suite:suite/slow\"");
    r.stdout_contains("\"timed_out\":true");
}

#[test]
fn cases_on_a_non_test_target_are_refused() {
    let p = Project::new("cases-wrong-kind");
    p.write("dowel.toml", "[package]\nname    = \"p\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.cases]\none = { args = [] }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-block");
    r.stderr_contains("only `test` targets register cases");
}

#[test]
fn an_unknown_case_property_gets_a_suggestion() {
    let p = case_project("cases-typo", "one = { timout = 5 }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-property");
    r.stderr_contains("did you mean `timeout`?");
}

/// `[test.<name>.harness]` — 実行ファイル自身に事例を列挙させる（ADR-0023）。
///
/// dowel は枠組みを1つも知らない。「どう尋ねるか」だけをマニフェストから読む。
fn harness_project(name: &str, harness: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"suite\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        &format!("[test.suite]\nsources = glob(\"tests/*.c\")\n\n[test.suite.harness]\n{harness}"),
    );
    // `--list` で名前を1行ずつ、`--run <名前>` で1件だけ走らせる小さな枠組み。
    p.write(
        "tests/suite.c",
        r#"#include <stdio.h>
#include <string.h>
static const char *CASES[] = { "adds", "subtracts", "divides" };
int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "--list") == 0) {
        for (unsigned i = 0; i < sizeof CASES / sizeof *CASES; i++) {
            printf("%s\n", CASES[i]);
        }
        return 0;
    }
    if (argc > 2 && strcmp(argv[1], "--run") == 0) {
        if (strcmp(argv[2], "divides") == 0) { return 1; }   /* この1件だけ落ちる */
        printf("ran %s\n", argv[2]);
        return 0;
    }
    if (argc > 1 && strcmp(argv[1], "--empty") == 0) { return 0; }
    if (argc > 1 && strcmp(argv[1], "--broken") == 0) { return 2; }
    return 0;
}
"#,
    );
    p
}

#[test]
fn the_binary_is_asked_what_cases_it_contains() {
    let p = harness_project("harness-basic", "list = [\"--list\"]\nrun  = [\"--run\"]\n");
    let r = p.run(".", &["test"]);
    // `divides` だけが落ちる。列挙も選択も効いている証拠になる。
    r.failure();
    r.stderr_contains("running 3 tests");
    r.stderr_contains("suite:suite/adds ... ok");
    r.stderr_contains("suite:suite/subtracts ... ok");
    r.stderr_contains("suite:suite/divides ... FAILED");
}

#[test]
fn a_harness_that_lists_nothing_is_a_failure_not_a_silent_pass() {
    // 列挙できないことと事例が無いことは別である。黙って0件成功にすると、
    // 試験が消えたことに誰も気づかない。
    let p = harness_project("harness-empty", "list = [\"--empty\"]\n");
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("could not list the cases");
    r.stderr_contains("listed no cases");
}

#[test]
fn a_listing_that_fails_says_so() {
    let p = harness_project("harness-broken", "list = [\"--broken\"]\n");
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("could not list the cases");
    r.stderr_contains("status 2");
}

#[test]
fn harness_level_options_reach_every_discovered_case() {
    let p = harness_project(
        "harness-options",
        "list   = [\"--list\"]\nrun    = [\"--run\"]\nlabels = [\"unit\"]\n",
    );
    // 宣言した名前で全ての事例が選ばれる。
    let r = p.run(".", &["test", "--label=unit"]);
    r.failure();
    r.stderr_contains("running 3 tests");
    // 名乗っていない名前では1件も選ばれない。
    let r = p.run(".", &["test", "--label=slow"]);
    r.success();
    r.stderr_contains("no test carries `slow`");
}

#[test]
fn discovered_cases_are_rerun_individually_by_failed() {
    let p = harness_project("harness-failed", "list = [\"--list\"]\nrun = [\"--run\"]\n");
    p.run(".", &["test"]).failure();
    let r = p.run(".", &["test", "--failed"]);
    r.failure();
    r.stderr_contains("running 1 test");
    r.stderr_contains("suite:suite/divides");
}

#[test]
fn a_harness_needs_to_say_how_to_list() {
    // 既定を当てない。当てると「どう尋ねたのか」がマニフェストから読めない。
    let p = harness_project("harness-no-list", "run = [\"--run\"]\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("missing-field");
    r.stderr_contains("`list`");
}

#[test]
fn cases_and_harness_cannot_both_be_declared() {
    let p = harness_project("harness-and-cases", "list = [\"--list\"]\n");
    p.write(
        "dowel.build",
        "[test.suite]\nsources = glob(\"tests/*.c\")\n\n[test.suite.cases]\none = { args = [] }\n\n[test.suite.harness]\nlist = [\"--list\"]\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("conflicting-declaration");
    r.stderr_contains("both answer what the cases");
}

#[test]
fn a_harness_on_a_non_test_target_is_refused() {
    let p = Project::new("harness-wrong-kind");
    p.write("dowel.toml", "[package]\nname    = \"p\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.harness]\nlist = [\"--list\"]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-block");
    r.stderr_contains("only `test` targets have a harness");
}

#[test]
fn an_unknown_harness_property_gets_a_suggestion() {
    let p = harness_project("harness-typo", "list = [\"--list\"]\nrunn = [\"--run\"]\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-property");
    r.stderr_contains("did you mean `run`?");
}

/// `dowel debug`（ADR-0024）。
///
/// デバッガはツールチェーンの道具の1つであり、トリプルごとに選ばれる。
/// スタブの立て方は宣言させる——推測すると「それらしく見えて固まる」列ができる。
fn debug_project(name: &str, extra_toml: &str, extra_build: &str) -> Project {
    let p = Project::new(name);
    p.write(
        "dowel.toml",
        &format!("[package]\nname    = \"app\"\nversion = \"0.1.0\"\n{extra_toml}"),
    );
    p.write(
        "dowel.build",
        &format!("[bin.app]\nsources = glob(\"src/*.c\")\n\n[lib.helper]\nsources = glob(\"src/*.c\")\n{extra_build}"),
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");
    p
}

#[test]
fn the_launch_configuration_names_the_program_the_debugger_and_the_directory() {
    let p = debug_project("debug-dap", "", "");
    let r = p.run(".", &["debug", "app", "--dap"]);
    r.success();
    // 構成は成果物なので stdout。
    r.stdout_contains("\"type\": \"cppdbg\"");
    r.stdout_contains("\"miDebuggerPath\": \"gdb\"");
    r.stdout_contains("bin/app");
    // ホストでは繋ぎ先が無い。書くと、無い相手を待つ構成になる。
    assert!(!r.stdout.contains("miDebuggerServerAddress"), "a host session named a stub\n{r}");
    // 実際に組んでいる。構成だけ出して成果物が無いのでは、開いても始まらない。
    assert!(build_dir(&p.path("."), "debug").join("bin/app").exists());
}

#[test]
fn the_declared_debugger_is_the_one_named() {
    // `[toolchain] debug` が効く。道具の表に載っているので、トリプルごとの
    // 宣言もそのまま効く。
    let p = debug_project("debug-declared", "\n[toolchain]\ndebug = \"lldb\"\n", "");
    let r = p.run(".", &["debug", "app", "--dap"]);
    r.success();
    r.stdout_contains("\"miDebuggerPath\": \"lldb\"");
}

#[test]
fn a_library_has_nothing_to_start() {
    let p = debug_project("debug-lib", "", "");
    let r = p.run(".", &["debug", "helper"]);
    r.failure();
    r.stderr_contains("not-debuggable");
    r.stderr_contains("nothing to start");
}

#[test]
fn a_cross_session_without_a_declared_stub_is_refused() {
    // ホストの gdb を別アーキテクチャの実行ファイルに向けても、読めるのは
    // 記号までである。断って、何を宣言すればよいかを述べる。
    let p = debug_project(
        "debug-cross-nostub",
        "\n[toolchain.riscv64gc-unknown-linux-gnu]\nc = \"cc\"\n",
        "\n[runner.riscv64gc-unknown-linux-gnu]\ncommand = \"true\"\n",
    );
    let r = p.run(".", &["debug", "app", "--target=riscv64gc-unknown-linux-gnu"]);
    r.failure();
    r.stderr_contains("missing-debug-stub");
    r.stderr_contains("debug_args");
    r.stderr_contains("debug_connect");
}

#[test]
fn a_declared_stub_reaches_the_launch_configuration() {
    let p = debug_project(
        "debug-cross-stub",
        "\n[toolchain.riscv64gc-unknown-linux-gnu]\nc = \"cc\"\ndebug = \"riscv64-linux-gnu-gdb\"\n",
        "\n[runner.riscv64gc-unknown-linux-gnu]\ncommand       = \"qemu-riscv64\"\nargs          = [\"-L\", \"/usr/riscv64-linux-gnu\"]\ndebug_args    = [\"-g\", \"1234\"]\ndebug_connect = \"localhost:1234\"\n",
    );
    let r = p.run(".", &["debug", "app", "--target=riscv64gc-unknown-linux-gnu", "--dap"]);
    r.success();
    r.stdout_contains("\"miDebuggerServerAddress\": \"localhost:1234\"");
    r.stdout_contains("\"debugServerPath\": \"qemu-riscv64\"");
    // ツールチェーンの gdb が選ばれる。ホストのものではない。
    r.stdout_contains("riscv64-linux-gnu-gdb");
    // スタブの引数は成果物の**前**。qemu も gdbserver も自分の引数を先に取る。
    let args = &r.stdout[r.stdout.find("debugServerArgs").expect("the stub args are missing")..];
    let g = args.find("\"-g\"").expect("`-g` is not among the stub args");
    let bin = args.find("bin/app\"").expect("the artifact is not among the stub args");
    assert!(g < bin, "the stub arguments came after the artifact\n{r}");
}

#[test]
fn a_debugger_that_is_not_installed_is_reported_before_starting() {
    let p = debug_project("debug-missing", "\n[toolchain]\ndebug = \"no-such-debugger\"\n", "");
    let r = p.run(".", &["debug", "app"]);
    r.failure();
    r.stderr_contains("missing-toolchain");
    r.stderr_contains("no-such-debugger");
    // 逃げ道を述べる。構成だけなら道具が無くても出せる。
    r.stderr_contains("--dap");
}

#[test]
fn debug_takes_exactly_one_target() {
    let p = debug_project("debug-arity", "", "");
    let r = p.run(".", &["debug"]);
    r.failure();
    r.stderr_contains("one target");
}
