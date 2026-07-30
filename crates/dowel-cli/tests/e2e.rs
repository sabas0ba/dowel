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
fn a_header_change_is_seen_after_building_with_the_other_executor() {
    // issue #41: ninja で組んだツリーを direct で組み直す。依存の記録が
    // 実行器の実装詳細に畳まれていると、ヘッダの変更が黙って見落とされ、
    // 古い成果物が残る。
    let p = two_package_project("cross-executor-header");
    p.run("app", &["build"]).success(); // 既定の ninja
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");

    p.write(
        "libfoo/src/internal.h",
        "#pragma once\n#define FOO_BIAS 100\nstatic inline int bias(void) { return FOO_BIAS; }\n",
    );
    let r = p.run("app", &["build", "--executor=direct", "--log-level=trace"]);
    r.success();
    assert!(!r.stderr.contains("ran 0 actions"), "the header change did not propagate\n{r}");
    assert_eq!(run_artifact(&bin), "sum=105 opt=0 api=1\n");
}

#[test]
fn the_artifact_is_up_to_date_after_crossing_executors() {
    // issue #41 の裏面。何も変えずに実行器を替えただけなら、全てを
    // 作り直すのではなく最新と判定される。依存の記録（depfile）が
    // 実行器を跨いで残っていることの検査である。
    let p = two_package_project("cross-executor-clean");
    p.run("app", &["build"]).success(); // 既定の ninja
    let r = p.run("app", &["build", "--executor=direct", "--log-level=debug"]);
    r.success().stderr_contains("ran 0 actions");
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
    p.run("app", &["build", "--executor=direct"]).success();
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
