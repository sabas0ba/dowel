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
fn the_direct_executor_produces_the_same_artifact() {
    let p = two_package_project("direct");
    p.run("app", &["build", "--executor=direct"]).success();
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
    let r = p.run("app", &["build", "--executor=direct"]);
    r.failure();
    assert!(r.stderr.contains("internal.h"), "the compiler diagnostic is not visible\n{r}");
}

#[test]
fn a_compile_failure_exits_nonzero_and_shows_the_cause() {
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
fn a_rebuild_runs_nothing() {
    let p = two_package_project("incremental");
    p.run("app", &["build", "--executor=direct"]).success();
    let second = p.run("app", &["build", "--executor=direct", "--log-level=trace"]);
    second.success().stderr_contains("ran 0 actions");
    // 何が最新と判定されたかが個別に見える。
    second.stderr_contains("up to date: CC ");
}

#[test]
fn touching_a_header_triggers_recompilation() {
    let p = two_package_project("depfile");
    p.run("app", &["build", "--executor=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");

    // ソースではなくヘッダだけを変える。depfile を読めていなければ再実行されない。
    p.write(
        "libfoo/src/internal.h",
        "#pragma once\n#define FOO_BIAS 100\nstatic inline int bias(void) { return FOO_BIAS; }\n",
    );
    let r = p.run("app", &["build", "--executor=direct", "--log-level=trace"]);
    r.success();
    assert!(!r.stderr.contains("ran 0 actions"), "the header change did not propagate\n{r}");
    // 再実行の理由が出ること。depfile 経由で拾ったヘッダが名指しされる。
    r.stderr_contains("stale: ");
    r.stderr_contains("internal.h");
    assert_eq!(run_artifact(&bin), "sum=105 opt=0 api=1\n");
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

    // ninja の作業ファイルはビルドディレクトリに閉じ込める。
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
fn unfetchable_dependencies_are_refused_explicitly() {
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
    // 起動してからでは `Exec format error` になり、構成の誤りが
    // テストの失敗として報告されてしまう。
    let p = Project::new("runner-missing");
    p.write("dowel.toml", "[package]\nname    = \"r\"\nversion = \"0.1.0\"\n");
    p.write("dowel.build", "[test.t]\nsources = glob(\"*.c\")\n");
    p.write("t.c", "int main(void) { return 0; }\n");

    // ビルドはホストのコンパイラで通るが、起動は拒まれる。
    let r = p.run(".", &["test", "--target=riscv64gc-unknown-linux-gnu", "--no-run"]);
    r.success();
    let r = p.run(".", &["test", "--target=riscv64gc-unknown-linux-gnu"]);
    r.failure();
    r.stderr_contains("missing-runner");
    r.stderr_contains("riscv64gc-unknown-linux-gnu");
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
