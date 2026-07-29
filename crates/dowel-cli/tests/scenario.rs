//! シナリオ。時間をまたぐ操作列に対する振る舞い。
//!
//! e2e が「1回の実行が正しいか」を見るのに対し、ここは
//! 「編集して、また実行して、また編集して」という実際の使われ方を見る。
//! ビルドシステムの価値の大半は2回目以降の実行にあり、そこは
//! 単発の実行をいくら並べても検査できない。
//!
//! 観測は `--executor=direct --log-level=debug` の判定理由による。
//! 成果物の更新時刻を見る方法もあるが、時刻の分解能に依存し、
//! 「なぜ再実行したか」が残らない。
//!
//! 設計は [`docs/51-testing.md`](../../../docs/51-testing.md) にある。

mod common;

use common::{build_dir, run_artifact, Project};

/// ライブラリ1つと、それを使う実行ファイル1つ。
///
/// シナリオは操作列そのものが検査対象なので、プロジェクトは最小にする。
/// 形の複雑さは実物フィクスチャ（`tests/projects/`）の担当である。
fn project(name: &str) -> Project {
    let p = Project::new(name);
    p.write("libfoo/dowel.toml", "[package]\nname    = \"libfoo\"\nversion = \"0.1.0\"\n");
    p.write(
        "libfoo/dowel.build",
        r#"
[lib.foo]
sources = glob("src/*.c")

[lib.foo.public]
includes = [dir("include")]

[lib.foo.private]
flags = ["-Wall"]

[test.foo_test]
sources = glob("tests/*.c")

[test.foo_test.private]
deps = [target("foo")]
"#,
    );
    p.write("libfoo/include/foo.h", "#pragma once\nint foo_one(void);\nint foo_two(void);\n");
    p.write("libfoo/src/one.c", "#include \"foo.h\"\nint foo_one(void) { return 1; }\n");
    p.write("libfoo/src/two.c", "#include \"foo.h\"\nint foo_two(void) { return 2; }\n");
    p.write(
        "libfoo/tests/foo_test.c",
        "#include \"foo.h\"\nint main(void) { return foo_one() + foo_two() == 3 ? 0 : 1; }\n",
    );

    p.write(
        "app/dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"libfoo\"\npath = \"../libfoo\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"libfoo\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"foo.h\"\n\
         int main(void) { printf(\"%d\\n\", foo_one() + foo_two()); return 0; }\n",
    );
    p
}

/// ビルドして、走ったコンパイル動作の記述を集める。
fn rebuild(p: &Project) -> Vec<String> {
    let r = p.run("app", &["build", "--executor=direct", "--log-level=debug"]);
    r.success();
    r.stderr
        .lines()
        .filter(|l| l.contains("CC ") || l.contains("AR ") || l.contains("LINK "))
        .filter(|l| !l.contains("up to date"))
        .map(|l| l.trim().to_string())
        .collect()
}

fn ran_nothing(p: &Project) {
    let r = p.run("app", &["build", "--executor=direct", "--log-level=debug"]);
    r.success().stderr_contains("ran 0 actions");
}

#[test]
fn editing_one_source_recompiles_only_that_object() {
    let p = project("scenario-edit-one");
    p.run("app", &["build", "--executor=direct"]).success();
    ran_nothing(&p);

    p.write("libfoo/src/one.c", "#include \"foo.h\"\nint foo_one(void) { return 10; }\n");
    let ran = rebuild(&p);

    let recompiled: Vec<&String> = ran.iter().filter(|l| l.contains("CC ")).collect();
    assert_eq!(recompiled.len(), 1, "expected exactly one recompile, got {ran:?}");
    assert!(recompiled[0].contains("one.c"), "{ran:?}");
    // 下流は作り直される。ライブラリと、それを使う実行ファイルの双方。
    assert!(ran.iter().any(|l| l.contains("AR ")), "the archive was not rebuilt: {ran:?}");
    assert!(ran.iter().any(|l| l.contains("LINK ")), "the binary was not relinked: {ran:?}");

    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "12\n");
}

#[test]
fn touching_a_public_header_recompiles_everything_that_includes_it() {
    let p = project("scenario-header");
    p.run("app", &["build", "--executor=direct"]).success();

    // 中身を変える。depfile を読めていなければ何も起きない。
    p.write(
        "libfoo/include/foo.h",
        "#pragma once\n/* touched */\nint foo_one(void);\nint foo_two(void);\n",
    );
    let ran = rebuild(&p);

    // ライブラリの2つのソースと、app の main.c。テストも含む。
    for source in ["one.c", "two.c", "main.c"] {
        assert!(
            ran.iter().any(|l| l.contains(source)),
            "`{source}` includes the header but was not recompiled: {ran:?}"
        );
    }
}

#[test]
fn adding_a_source_file_is_picked_up_without_touching_the_manifest() {
    // `glob` の展開は評価時ではなく plan 時に行う。逆にすると、
    // ファイルを追加してもマニフェストを変更するまでビルド対象に入らない。
    let p = project("scenario-add-source");
    p.run("app", &["build", "--executor=direct"]).success();

    p.write(
        "libfoo/include/foo.h",
        "#pragma once\nint foo_one(void);\nint foo_two(void);\nint foo_three(void);\n",
    );
    p.write("libfoo/src/three.c", "#include \"foo.h\"\nint foo_three(void) { return 3; }\n");
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"foo.h\"\n\
         int main(void) { printf(\"%d\\n\", foo_one() + foo_two() + foo_three()); return 0; }\n",
    );

    let ran = rebuild(&p);
    assert!(ran.iter().any(|l| l.contains("three.c")), "the new source was not built: {ran:?}");

    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "6\n");
}

#[test]
fn removing_a_source_file_drops_it_from_the_build() {
    let p = project("scenario-remove-source");
    p.run("app", &["build", "--executor=direct"]).success();

    std::fs::remove_file(p.path("libfoo/src/two.c")).expect("cannot remove the source");
    p.write("libfoo/include/foo.h", "#pragma once\nint foo_one(void);\n");
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"foo.h\"\n\
         int main(void) { printf(\"%d\\n\", foo_one()); return 0; }\n",
    );

    p.run("app", &["build", "--executor=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "1\n");

    // 消えたソースは計画から外れる。
    let r = p.run("app", &["graph", "--kind=action"]);
    r.success();
    assert!(!r.stdout.contains("two.c"), "the removed source is still in the action graph");
}

#[test]
fn changing_a_flag_in_the_manifest_recompiles_the_target() {
    let p = project("scenario-flag");
    p.run("app", &["build", "--executor=direct"]).success();

    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.private]\ndeps  = [dep(\"libfoo\")]\nflags = [\"-DEXTRA=1\"]\n",
    );
    let ran = rebuild(&p);
    assert!(ran.iter().any(|l| l.contains("main.c")), "the flag change did not rebuild: {ran:?}");
}

#[test]
fn switching_configuration_leaves_the_other_one_intact() {
    let p = project("scenario-config");
    p.run("app", &["build", "--executor=direct"]).success();
    p.run("app", &["build", "--executor=direct", "--config=release"]).success();

    // 構成ごとにビルドディレクトリが分かれているため、
    // 往復しても互いを作り直さない。
    p.run("app", &["build", "--executor=direct", "--log-level=debug"])
        .success()
        .stderr_contains("ran 0 actions");
    p.run("app", &["build", "--executor=direct", "--config=release", "--log-level=debug"])
        .success()
        .stderr_contains("ran 0 actions");

    for opt in ["debug", "release"] {
        let bin = build_dir(&p.path("app"), opt).join("bin/app");
        assert_eq!(run_artifact(&bin), "3\n", "the {opt} artifact is wrong");
    }
}

#[test]
fn a_broken_edit_fails_and_the_fix_restores_the_build() {
    // 直したあとに「前回の失敗が残っていて通らない」という状態にならないこと。
    let p = project("scenario-break-and-fix");
    p.run("app", &["build", "--executor=direct"]).success();

    p.write("libfoo/src/one.c", "#include \"foo.h\"\nint foo_one(void) { return nope; }\n");
    p.run("app", &["build", "--executor=direct"]).failure().stderr_contains("nope");

    p.write("libfoo/src/one.c", "#include \"foo.h\"\nint foo_one(void) { return 1; }\n");
    p.run("app", &["build", "--executor=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "3\n");
}

#[test]
fn a_syntax_error_in_the_manifest_does_not_destroy_the_previous_build() {
    let p = project("scenario-broken-manifest");
    p.run("app", &["build", "--executor=direct"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "3\n");

    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\"\n");
    p.run("app", &["build"]).failure();
    // 前回の成果物は残っている。壊れた編集が既にあるものを壊さないこと。
    assert_eq!(run_artifact(&bin), "3\n");
}

#[test]
fn the_failed_test_workflow_narrows_and_then_clears() {
    // 「落ちる → `--failed` で絞る → 直す → 判定が消える」という一連の流れ。
    let p = project("scenario-failed");
    p.write("libfoo/tests/foo_test.c", "int main(void) { return 1; }\n");

    let r = p.run("libfoo", &["test"]);
    r.failure().stderr_contains("test libfoo:foo_test ... FAILED");

    // 直す前は `--failed` が拾う。
    let r = p.run("libfoo", &["test", "--failed"]);
    r.failure().stderr_contains("test libfoo:foo_test ... FAILED");

    p.write("libfoo/tests/foo_test.c", "int main(void) { return 0; }\n");
    p.run("libfoo", &["test"]).success();

    // 通ったので、もう `--failed` の対象ではない。
    let r = p.run("libfoo", &["test", "--failed"]);
    r.success();
    assert!(
        !r.stderr.contains("foo_test ... FAILED"),
        "a fixed test is still listed as failed\n{r}"
    );
}

#[test]
fn a_second_run_of_check_reports_the_same_diagnostics() {
    // 増分のメモが効いても診断は消えない。プロセスを跨いだ再実行でも同じ。
    let p = project("scenario-repeat-check");
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\nnosuchprop = 1\n");

    let first = p.run("app", &["check", "--message-format=json"]);
    first.failure();
    let second = p.run("app", &["check", "--message-format=json"]);
    second.failure();
    assert_eq!(first.stdout, second.stdout, "diagnostics differ between two identical runs");
}

#[test]
fn the_generated_ninja_file_is_stable_across_runs() {
    // 生成が決定的であること。差分が出るとビルドが不必要に走り、
    // 内容を版管理に入れる利用者の環境で無関係な差分が出る。
    let p = project("scenario-deterministic");
    p.run("app", &["build"]).success();
    let path = build_dir(&p.path("app"), "debug").join("build.ninja");
    let first = std::fs::read_to_string(&path).expect("cannot read build.ninja");

    p.run("app", &["build"]).success();
    let second = std::fs::read_to_string(&path).expect("cannot read build.ninja");
    assert_eq!(first, second, "the generated ninja file is not deterministic");
}

// --- ストアへの入力の記録（docs/20-architecture.md 5.2）------------------

/// ストアが記録した入力の一覧。
fn recorded_inputs(p: &Project, pkg: &str) -> String {
    let path = p.path(pkg).join(".dowel/cache/v1/inputs");
    std::fs::read_to_string(&path).unwrap_or_default()
}

#[test]
fn a_run_records_the_manifests_it_read() {
    let p = project("scenario-inputs");
    p.run("app", &["check"]).success();

    let text = recorded_inputs(&p, "app");
    // 読んだのは app と libfoo の dowel.toml / dowel.build の4件。
    let lines = text.lines().filter(|l| !l.starts_with('#')).count();
    assert_eq!(lines, 4, "unexpected input records:\n{text}");
    assert!(text.contains("dowel.build"), "{text}");
    assert!(text.contains("libfoo"), "{text}");
}

#[test]
fn a_second_process_sees_the_previous_run_as_unchanged() {
    let p = project("scenario-inputs-unchanged");
    p.run("app", &["check"]).success();

    // 2回目のプロセス。前回の記録と突き合わせ、`stat` の一致で判定する。
    let r = p.run("app", &["check", "--log-level=trace"]);
    r.success();
    assert!(
        r.stderr.contains("UnchangedByStat"),
        "the second process did not reuse the recorded stat keys\n{r}"
    );
    assert!(!r.stderr.contains("Changed"), "nothing was edited\n{r}");
}

#[test]
fn editing_a_manifest_is_reported_as_changed_across_processes() {
    let p = project("scenario-inputs-changed");
    p.run("app", &["check"]).success();

    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.private]\ndeps  = [dep(\"libfoo\")]\nflags = [\"-DX=1\"]\n",
    );
    let r = p.run("app", &["check", "--log-level=trace"]);
    r.success();
    r.stderr_contains("Changed");
}

#[test]
fn rewriting_a_manifest_with_the_same_bytes_is_unchanged_across_processes() {
    // `stat` は動くが内容は同じ。内容の指紋で「変わっていない」と判定する。
    let p = project("scenario-inputs-same-bytes");
    p.run("app", &["check"]).success();

    let text = std::fs::read_to_string(p.path("app/dowel.build")).unwrap();
    p.write("app/dowel.build", &text);

    let r = p.run("app", &["check", "--log-level=trace"]);
    r.success();
    r.stderr_contains("UnchangedByContent");
}

#[test]
fn the_recorded_inputs_show_up_in_cache_info() {
    let p = project("scenario-inputs-cache-info");
    p.run("app", &["check"]).success();
    // 入力の記録はストアのディレクトリに置く。
    p.run("app", &["cache", "info"]).success().stdout_contains(".dowel/cache/v1");
    assert!(!recorded_inputs(&p, "app").is_empty());
}

// --- ストアからの評価結果の復元（ADR-0012）--------------------------------

/// 実行が書いた記録の要約。`store: wrote N values, restored M, skipped K …` を読む。
fn store_counts(stderr: &str) -> (usize, usize, usize) {
    let line = stderr
        .lines()
        .find(|l| l.contains("store: wrote"))
        .unwrap_or_else(|| panic!("no store summary in the log:\n{stderr}"));
    let n = |after: &str| -> usize {
        let rest = line.split(after).nth(1).expect("the summary changed shape");
        let word = rest.split_whitespace().next().unwrap_or("");
        // 数の直後に句読点が続く位置がある（`restored 0,`）。
        word.trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .unwrap_or_else(|_| panic!("`{word}` after `{after}` is not a count:\n{line}"))
    };
    (n("wrote"), n("restored"), n("skipped"))
}

/// 成否は問わない。ストアへの書き込みは読み込み直後に行うため、
/// 後段が失敗しても要約は出る。
fn run_and_count(p: &Project, pkg: &str) -> (usize, usize, usize) {
    store_counts(&p.run(pkg, &["check", "--log-level=debug"]).stderr)
}

#[test]
fn a_first_run_stores_every_manifest_it_evaluated() {
    let p = project("scenario-store-first");
    // app と libfoo の dowel.toml / dowel.build の4件。
    assert_eq!(run_and_count(&p, "app"), (4, 0, 0));
}

#[test]
fn an_unchanged_manifest_is_restored_from_the_store() {
    let p = project("scenario-store-restore");
    run_and_count(&p, "app");
    // 2回目のプロセス。本文が変わっていないため、解析も評価もしない。
    assert_eq!(run_and_count(&p, "app"), (0, 4, 0));
}

#[test]
fn editing_a_manifest_makes_the_store_recompute_it() {
    // 復元の検査だけでは、そもそも評価を問い合わせていない状態でも通る。
    let p = project("scenario-store-edit");
    run_and_count(&p, "app");

    let text = std::fs::read_to_string(p.path("app/dowel.build")).unwrap();
    p.write("app/dowel.build", &format!("{text}\n# a comment\n"));

    // 編集した1件だけを評価し直し、書き直す。残る3件は復元する。
    assert_eq!(run_and_count(&p, "app"), (1, 3, 0));
}

#[test]
fn a_manifest_with_diagnostics_is_not_stored() {
    let p = project("scenario-store-diagnostics");
    run_and_count(&p, "app");

    let text = std::fs::read_to_string(p.path("app/dowel.build")).unwrap();
    p.write("app/dowel.build", &format!("{text}\n[bin.app]\nbogus_property = 1\n"));
    // 誤りのあるファイルは格納しない。残る3件は復元する。
    assert_eq!(run_and_count(&p, "app"), (0, 3, 1));

    // 直せば格納される。格納しない判断が誤りの残っている間だけであることを見る。
    // 元の本文へ戻すと最初の実行の記録に当たるため、別の妥当な本文にする。
    p.write("app/dowel.build", &format!("{text}\n# fixed\n"));
    assert_eq!(run_and_count(&p, "app"), (1, 3, 0));
}

#[test]
fn a_restored_run_produces_the_same_plan() {
    // 復元は速度のためのものであり、結果を変えてはならない。
    let p = project("scenario-store-same-plan");
    let first = p.run("app", &["graph", "--kind=action", "--format=json"]);
    first.success();
    let second = p.run("app", &["graph", "--kind=action", "--format=json"]);
    second.success();
    assert_eq!(first.stdout, second.stdout, "the restored run produced a different plan");
}

#[test]
fn a_diagnostic_raised_outside_the_evaluation_survives_the_restore() {
    // 機能名の検証は評価の外で走る（`dowel.build` 単体では値域が分からない）。
    // 評価結果に診断が無いためファイルは格納される。復元した文書が
    // 構成参照を持たなければ、2回目の実行で診断が消える。
    let p = project("scenario-store-outside-diagnostic");
    // 既存のブロックへ足す。表を増やすと `duplicate-table` が出て、
    // ファイルが格納されなくなり復元の経路を通らない。
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.private]\ndeps  = [dep(\"libfoo\")]\nflags = [\"-DX\"] when feature.nope\n",
    );

    let first = p.run("app", &["check", "--message-format=json", "--log-level=debug"]);
    assert!(first.stdout.contains("unknown-feature"), "the first run did not report it\n{first}");
    assert_eq!(store_counts(&first.stderr).0, 4, "the file was not stored\n{first}");

    let second = p.run("app", &["check", "--message-format=json", "--log-level=debug"]);
    assert_eq!(store_counts(&second.stderr).1, 4, "the file was not restored\n{second}");
    assert_eq!(first.stdout, second.stdout, "the restored run lost a diagnostic");
}

#[test]
fn the_nesting_limit_is_configurable_and_is_part_of_the_store_key() {
    // 生成されたマニフェストが既定の 64 段を超える場合の逃げ道（`--max-nesting`）。
    // 上限は評価結果の指紋に混ざる。混ざらないと、上げた上限で評価・格納した
    // 結果を既定の実行が復元してしまい、出るはずの診断が消える。
    let p = Project::new("scenario-max-nesting");
    p.write("app/dowel.toml", "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    p.write(
        "app/dowel.build",
        &format!(
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\nflags = {}{}\n",
            "[".repeat(100),
            "]".repeat(100)
        ),
    );

    let refused = p.run("app", &["check", "--message-format=json"]);
    assert!(refused.stdout.contains("nesting-too-deep"), "{refused}");

    let raised = p.run("app", &["check", "--message-format=json", "--max-nesting=128"]);
    assert!(!raised.stdout.contains("nesting-too-deep"), "{raised}");

    // 既定へ戻した実行でも診断は出る。上げた上限で格納した結果を復元しない。
    let back = p.run("app", &["check", "--message-format=json"]);
    assert!(back.stdout.contains("nesting-too-deep"), "the store masked the diagnostic\n{back}");
}

#[test]
fn removing_the_store_falls_back_to_evaluating_everything() {
    // ストアは高速化のためのものであり、無くても結果は変わらない。
    let p = project("scenario-store-removed");
    let before = p.run("app", &["graph", "--kind=action", "--format=json"]);
    before.success();
    std::fs::remove_dir_all(p.path("app/.dowel")).expect("cannot remove the store");
    assert_eq!(run_and_count(&p, "app"), (4, 0, 0));
    let after = p.run("app", &["graph", "--kind=action", "--format=json"]);
    assert_eq!(before.stdout, after.stdout, "the plan changed after the store was removed");
}
