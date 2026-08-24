//! e2e。実際に C をコンパイルし、リンクし、実行する。
//!
//! 単体テストが「アクショングラフが期待通りか」を見るのに対し、ここは
//! 「そのグラフを実行すると本当に動く実行ファイルができるか」を見る。
//! 2つの間には、フラグの引用、インクルード探索の順序、リンク順、
//! 再ビルドの判定といった、机上では落ちない差がある。

mod common;

use std::io::BufRead;

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

/// direct バックエンドは `--jobs` を受け取り、順序を守ったまま同時に走らせる
/// （[ADR-0056](../../../docs/adr/0056-direct-backend-parallelism.md)）。
///
/// 順序の誤りは競合なので、1回の実行で必ず出るとは限らない。ここが見るのは
/// 「同時に走らせても同じものが出来る」ことと、2回目に何も走らないこと——
/// 前提を無視して走らせれば、どちらかが崩れる。
#[test]
fn the_direct_backend_builds_in_parallel_and_still_gets_the_same_artifact() {
    let p = two_package_project("direct-jobs");
    let r = p.run("app", &["build", "--backend=direct", "--jobs=4", "--log-level=debug"]);
    r.success().stderr_contains("with 4 job(s)");
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 opt=0 api=1\n");

    let second = p.run("app", &["build", "--backend=direct", "--jobs=4", "--log-level=debug"]);
    second.success().stderr_contains("ran 0 steps");
}

/// 生成が翻訳より先に走ること。同時に走らせても変わらない
/// （ADR-0054 の順序を、ADR-0056 の走らせ方で確かめる）。
#[test]
fn a_generation_still_precedes_the_compiles_when_jobs_run_in_parallel() {
    if !program_exists("sh") {
        eprintln!("skipping: sh is not on PATH");
        return;
    }
    let p = Project::new("direct-jobs-generate");
    p.write("dowel.toml", "[package]\nname = \"app\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.generate]\n\
         limits = { command = \"sh\", args = [file(\"gen.sh\")], \
         inputs = [file(\"src/limit.txt\")], outputs = [\"limits.h\"] }\n",
    );
    p.write("gen.sh", "set -eu\nprintf '#define LIMIT %s\\n' \"$(cat \"$1\")\" > limits.h\n");
    p.write("src/limit.txt", "5\n");
    // 翻訳単位を増やして、生成と同時に走りうる相手を作る。
    for i in 0..6 {
        p.write(
            &format!("src/part{i}.c"),
            &format!("#include \"limits.h\"\nint part{i}(void) {{ return LIMIT; }}\n"),
        );
    }
    p.write(
        "src/main.c",
        "#include <stdio.h>\n#include \"limits.h\"\nint part0(void);\n\
         int main(void) { printf(\"%d %d\\n\", LIMIT, part0()); return 0; }\n",
    );

    p.run(".", &["build", "--backend=direct", "--jobs=8"]).success();
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/app")), "5 5\n");
}

/// どのバックエンドで組んでも、走ったステップが1行ずつ見える
/// （[ADR-0057](../../../docs/adr/0057-progress-is-shown-while-it-runs.md)）。
///
/// direct は `log_info!` で述べていた——既定のログ水準は `warn` なので、
/// 誰にも届いていなかった。ninja が居ない機械ではそこが既定である。
#[test]
fn every_backend_that_builds_shows_one_line_per_step() {
    for backend in ["ninja", "direct", "make"] {
        if backend != "direct" && !program_exists(backend) {
            eprintln!("skipping {backend}: it is not on PATH");
            continue;
        }
        let p = two_package_project(&format!("progress-{backend}"));
        // ログ水準は既定のまま。見えるかどうかがここの検査対象である。
        let r = p.run("app", &["build", &format!("--backend={backend}")]);
        r.success();
        assert!(r.stderr.contains("CC "), "{backend} showed no compile\n{r}");
        assert!(r.stderr.contains("LINK "), "{backend} showed no link\n{r}");
        // 走らせる側が段数を持つなら、それも見える。
        if backend != "make" {
            assert!(r.stderr.contains("[1/"), "{backend} showed no count\n{r}");
        }
    }
}

/// 黙らせる術は1つだけである（ADR-0057）。
#[test]
fn only_the_off_level_silences_progress() {
    let p = two_package_project("progress-off");
    let r = p.run("app", &["build", "--backend=direct", "--log-level=off"]);
    r.success();
    assert!(!r.stderr.contains("CC "), "`off` still showed progress\n{r}");
}

/// 進捗は走っている**間**に出る。溜めてから出していた頃は、1.3 秒のビルドの
/// 11 行が最後の 19ms に固まって現れた
/// （[ADR-0057](../../../docs/adr/0057-progress-is-shown-while-it-runs.md)）。
///
/// 速い生成と遅い生成を **2本並べて**走らせ、速い方の行が遅い方の眠りの間に
/// 届くことを見る。「終わる前に届いた」では足りない——溜めてから出す実装でも、
/// 出すのは終了の直前だからである。
#[test]
fn progress_appears_while_the_build_is_still_running() {
    if !program_exists("sh") {
        eprintln!("skipping: sh is not on PATH");
        return;
    }
    // 遅い方が眠る長さ。1行目はこれより前に来なければならない。
    const SLOW: std::time::Duration = std::time::Duration::from_secs(3);

    for backend in ["direct", "ninja", "make"] {
        if backend != "direct" && !program_exists(backend) {
            eprintln!("skipping {backend}: it is not on PATH");
            continue;
        }
        let p = Project::new(&format!("progress-live-{backend}"));
        p.write("dowel.toml", "[package]\nname = \"app\"\nversion = \"0\"\n");
        p.write(
            "dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
             [bin.app.generate]\n\
             quick = { command = \"sh\", args = [file(\"quick.sh\")], outputs = [\"a.c\"] }\n\
             slow = { command = \"sh\", args = [file(\"slow.sh\")], outputs = [\"b.c\"] }\n",
        );
        p.write("quick.sh", "printf 'int a(void){return 1;}\\n' > a.c\n");
        p.write("slow.sh", "sleep 3\nprintf 'int b(void){return 2;}\\n' > b.c\n");
        p.write("src/main.c", "int a(void);\nint b(void);\nint main(void){return a()+b()-3;}\n");

        // 2本で走らせる。どちらを先に選ぶかは走らせる側の裁量であり、
        // 1本にすると ninja と make で順序を仮定することになる。
        let start = std::time::Instant::now();
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_dowel"))
            .args(["build", &format!("--backend={backend}"), "--jobs=2"])
            .current_dir(&p.root)
            .env_remove("DOWEL_LOG")
            .env_remove("DOWEL_CACHE")
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("cannot start dowel");
        let stderr = child.stderr.take().expect("stderr is piped");
        let mut lines = std::io::BufReader::new(stderr).lines();
        let first = lines
            .by_ref()
            .map_while(Result::ok)
            .find(|l| l.contains("GEN "))
            .unwrap_or_else(|| panic!("{backend} wrote no progress line"));
        let at_first = start.elapsed();
        // 残りは読み切る。読み手が居なくなると子は stderr に書けなくなる。
        for _ in lines.by_ref().map_while(Result::ok) {}
        let status = child.wait().expect("cannot wait for dowel");
        let total = start.elapsed();

        assert!(status.success(), "the {backend} build failed");
        assert!(
            total >= SLOW,
            "{backend}: the slow generation did not run; this proves nothing ({total:?})"
        );
        assert!(
            at_first < SLOW,
            "{backend}: the first progress line ({first}) arrived after {at_first:?}, \
             with the slow step sleeping {SLOW:?} — it was buffered, not live"
        );
    }
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
    // 語彙が閉じていることが出力自体から分かる（ADR-0034、issue #143）。
    // 読むのは道具であり、「暫定」と言われた語彙を当てにする理由は無い。
    r.stdout_contains("ADR-0034");
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
    r.stderr_contains("accepts: c, cxx, asm, ar");
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

/// 書庫の依存（[ADR-0029](../../../docs/adr/0029-tarball-dependencies.md)）。
///
/// 上流は木の中に作って `file://` で指す。ネットワークに出ずに、取得・検証・
/// 展開の経路をそのまま通せる。
fn tarball_remote(p: &Project) -> (String, String) {
    p.write(
        "upstream/mylib-1.0/dowel.toml",
        "[package]\nname    = \"mylib\"\nversion = \"1.0.0\"\n",
    );
    p.write(
        "upstream/mylib-1.0/dowel.build",
        "[lib.mylib]\nsources = glob(\"src/*.c\")\n\n[lib.mylib.public]\nincludes = [dir(\"include\")]\n",
    );
    p.write("upstream/mylib-1.0/include/mylib.h", "int mylib_answer(void);\n");
    p.write(
        "upstream/mylib-1.0/src/mylib.c",
        "#include \"mylib.h\"\nint mylib_answer(void) { return 42; }\n",
    );

    let out = std::process::Command::new("tar")
        .args(["czf", "mylib-1.0.tar.gz", "mylib-1.0"])
        .current_dir(p.path("upstream"))
        .output()
        .expect("cannot run tar");
    assert!(out.status.success(), "tar failed: {}", String::from_utf8_lossy(&out.stderr));

    let archive = p.path("upstream/mylib-1.0.tar.gz");
    let hash = dowel_support::sha256::hex_of_file(&archive).expect("cannot hash the archive");
    (format!("file://{}", archive.display()), hash)
}

fn write_tarball_manifest(p: &Project, url: &str, sha256: &str) {
    p.write(
        "app/dowel.toml",
        &format!(
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname   = \"mylib\"\nurl    = \"{url}\"\nsha256 = \"{sha256}\"\n"
        ),
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"mylib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"mylib.h\"\nint main(void) { printf(\"n=%d\\n\", mylib_answer()); return 0; }\n",
    );
}

#[test]
fn an_archive_dependency_is_fetched_verified_and_reused_offline() {
    let p = Project::new("tarball-dep");
    let (url, hash) = tarball_remote(&p);
    write_tarball_manifest(&p, &url, &hash);

    p.run("app", &["build"]).success();
    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "n=42\n");

    // 置き場は git の checkout と同じ形。指紋の先頭12桁で分け、完了印を持つ。
    let dir = p.path("app/.dowel/deps").join(format!("mylib-{}", &hash[..12]));
    assert!(dir.join(".dowel-rev").exists(), "missing {}", dir.display());
    // 書庫が包んでいた1階層は剥がれている。
    assert!(dir.join("dowel.toml").exists(), "the wrapping directory was not stripped");

    // 上流を消しても再ビルドできる。内容で固定されているため、2回目以降は
    // 取りに行かない。
    std::fs::remove_dir_all(p.path("upstream")).unwrap();
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"mylib.h\"\nint main(void) { printf(\"m=%d\\n\", mylib_answer()); return 0; }\n",
    );
    p.run("app", &["build"]).success();
    assert_eq!(run_artifact(&bin), "m=42\n");
}

#[test]
fn an_archive_whose_contents_changed_is_refused() {
    // URL は名前であって固定ではない。同じ名前の裏で中身が差し替わることは
    // 実際に起きる（ADR-0029）。
    let p = Project::new("tarball-dep-swapped");
    let (url, hash) = tarball_remote(&p);
    write_tarball_manifest(&p, &url, &hash);

    // 上流を書き換えて詰め直す。宣言の指紋はそのまま。
    p.write(
        "upstream/mylib-1.0/src/mylib.c",
        "#include \"mylib.h\"\nint mylib_answer(void) { return 7; }\n",
    );
    let out = std::process::Command::new("tar")
        .args(["czf", "mylib-1.0.tar.gz", "mylib-1.0"])
        .current_dir(p.path("upstream"))
        .output()
        .expect("cannot run tar");
    assert!(out.status.success());

    let r = p.run("app", &["build"]);
    r.failure();
    r.stderr_contains("unfetchable-dependency");
    r.stderr_contains("does not match its declared hash");
    // 期待と実際の両方を出す。片方だけでは何を貼り直せばよいか読めない。
    r.stderr_contains(&hash);
    // 検証は展開の前。中身は置かれていない。
    let dir = p.path("app/.dowel/deps").join(format!("mylib-{}", &hash[..12]));
    assert!(!dir.exists(), "the archive was unpacked despite the mismatch");
}

#[test]
fn an_archive_without_a_hash_is_refused() {
    // `rev` の無い git 依存と同じ扱い。URL だけでは固定にならない。
    let p = Project::new("tarball-dep-unpinned");
    let (url, _) = tarball_remote(&p);
    p.write(
        "app/dowel.toml",
        &format!(
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"mylib\"\nurl  = \"{url}\"\n"
        ),
    );
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("unpinned-dependency");
    r.stderr_contains("pinned by its contents");

    // 指紋の形が違う場合も同じく断る。
    p.write(
        "app/dowel.toml",
        &format!(
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname   = \"mylib\"\nurl    = \"{url}\"\nsha256 = \"deadbeef\"\n"
        ),
    );
    let short = p.run("app", &["check"]);
    short.failure();
    short.stderr_contains("is not a sha256 digest");
    short.stderr_contains("64 hexadecimal digits");
}

#[test]
fn an_archive_and_another_source_together_are_refused() {
    // 出所を2つ名乗る項目は、片方が読まれない（issue #79 と同じ規則）。
    let p = Project::new("tarball-dep-conflict");
    let (url, hash) = tarball_remote(&p);
    p.write(
        "app/dowel.toml",
        &format!(
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname   = \"mylib\"\nurl    = \"{url}\"\nsha256 = \"{hash}\"\npath   = \"../upstream/mylib-1.0\"\n"
        ),
    );
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("conflicting-dependency-source");
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

#[test]
fn an_imported_target_says_it_is_unverified_until_a_person_says_otherwise() {
    // 下書きは検査に落ちない——通る。落ちるのはリンクの段で、`deps` に
    // なっていないリンク入力について、リンカが未定義参照として述べる。
    // 弱める検査が無いので、印は provenance の宣言である（ADR-0053）。
    let p = Project::new("unverified-import");
    p.write("dowel.toml", "[package]\nname = \"u\"\nversion = \"0\"\n");
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\nunverified = true\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    // 警告であり、失敗ではない。下書きは組めて走る。
    let r = p.run(".", &["check"]);
    r.success();
    r.stderr_contains("unverified-import");
    r.stderr_contains("u:app");
    r.stderr_contains("migrate verify");
    p.run(".", &["build"]).success();

    // 印を外せば黙る。外すのは人である。
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    let r = p.run(".", &["check"]);
    r.success();
    assert!(!r.stderr.contains("unverified-import"), "the mark outlived the line:\n{}", r.stderr);
}

#[test]
fn migrate_verify_counts_the_targets_still_marked_unverified() {
    // 等価であることは下書きが完成したという意味ではない。見ているのは
    // 翻訳の引数であって、リンクの入力ではない（ADR-0053）。移植の単位は
    // 目標なので、残りも目標で数える。
    let p = two_package_project("migrate-verify-unverified");
    let build_file = std::fs::read_to_string(p.path("app/dowel.build")).unwrap();
    p.write(
        "app/dowel.build",
        &build_file.replace("[bin.app]\n", "[bin.app]\nunverified = true\n"),
    );
    p.run("app", &["build"]).success();
    let compdb = std::fs::read_to_string(p.path("app/compile_commands.json")).unwrap();
    p.write("ref.json", &compdb);

    let r = p.run("app", &["migrate", "verify", "../ref.json"]);
    r.success();
    // 等価と、残っていることは両立する。それが正直な状態である。
    r.stdout_contains("2 equivalent, 0 differing");
    r.stdout_contains("1 target(s) still marked");
    r.stdout_contains("app:app");

    let r = p.run("app", &["migrate", "verify", "../ref.json", "--format=json"]);
    r.success();
    r.stdout_contains("\"unverified\"");
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
    // 見出しのコメントは人だけが読む。機械が読める印も目標ごとに置く
    // （ADR-0053）——`check` が述べ、`migrate verify` が数える。
    assert_eq!(build_file.matches("unverified = true").count(), 2, "{build_file}");
    let r = p.run(".", &["check"]);
    r.success();
    r.stderr_contains("unverified-import");

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

/// Meson の introspect からの取り込み（docs/40-migration.md 4節）。
#[test]
fn migrate_import_drafts_manifests_from_meson_introspection() {
    let p = Project::new("meson-import");
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

    // meson が `meson setup` で自ら書くもの。引数は仕分けられておらず、
    // 1つの配列で来る——そこが CMake の reply と一番違う。
    p.write(
        "build/meson-info/intro-projectinfo.json",
        r#"{"version": "1.2.3", "descriptive_name": "demo", "subprojects": []}"#,
    );
    p.write(
        "build/meson-info/intro-targets.json",
        &format!(
            r#"[
              {{"name": "len", "type": "static library", "defined_in": "{src}/meson.build",
                "subproject": null,
                "target_sources": [{{"language": "c", "compiler": ["cc"],
                  "parameters": ["-I{src}/lib", "-DLIMIT=64", "-Wall", "-O2", "-g"],
                  "sources": ["{src}/lib/len.c"], "generated_sources": []}}]}},
              {{"name": "app", "type": "executable", "defined_in": "{src}/meson.build",
                "subproject": null,
                "target_sources": [{{"language": "c", "compiler": ["cc"],
                  "parameters": ["-I{src}/lib", "-Wall"],
                  "sources": ["{src}/src/main.c"], "generated_sources": []}}]}},
              {{"name": "docs", "type": "custom", "defined_in": "{src}/meson.build",
                "subproject": null, "target_sources": []}}
            ]"#
        ),
    );

    let r = p.run(".", &["migrate", "import", "build"]);
    r.success();
    r.stderr_contains("imported 2 target(s)");
    r.stderr_contains("UNVERIFIED");

    let manifest = std::fs::read_to_string(p.path("dowel.toml")).unwrap();
    assert!(manifest.contains("name    = \"demo\""), "{manifest}");
    // 印は Meson から来たことを述べる。CMake の文言のままにしない。
    assert!(manifest.contains("Meson configuration"), "{manifest}");

    let build_file = std::fs::read_to_string(p.path("dowel.build")).unwrap();
    assert!(build_file.contains("[lib.len]"), "{build_file}");
    assert!(build_file.contains("[bin.app]"), "{build_file}");
    // 落としたリンク入力を持つのはこちらである。印はそこにこそ要る
    // （ADR-0053）——`deps` が空のままでも `check` は通ってしまう。
    assert_eq!(build_file.matches("unverified = true").count(), 2, "{build_file}");
    // `custom` は組めない。読み飛ばす。
    assert!(!build_file.contains("docs"), "{build_file}");
    // 1つの配列が仕分けられている。
    assert!(build_file.contains("includes = [dir(\"lib\")]"), "{build_file}");
    assert!(build_file.contains("LIMIT = 64"), "{build_file}");
    let flags_line = build_file
        .lines()
        .find(|l| l.trim_start().starts_with("flags"))
        .expect("the draft declares flags");
    assert!(flags_line.contains("-Wall"), "{flags_line}");
    // 構成レベルのものは写らない。`-O2` / `-g` は `cfg.opt` の担当である。
    assert!(!flags_line.contains("-O2") && !flags_line.contains("\"-g\""), "{flags_line}");

    // 下書きがそのまま読める。組むには依存を人が書き足す必要がある——
    // introspect にリンク先が無いためで、それは下書きの限界として残る。
    p.run(".", &["check"]).success();
}

#[test]
fn migrate_import_says_what_to_run_when_it_finds_neither() {
    // 渡す先を間違えたときに、何を渡せばよいかが読めること。
    let p = Project::new("import-neither");
    p.write("build/CMakeCache.txt", "# not the File API\n");
    let r = p.run(".", &["migrate", "import", "build"]);
    r.failure();
    r.stderr_contains("neither a CMake File API reply nor Meson introspection");
    r.stderr_contains("codemodel-v2");
    r.stderr_contains("meson setup");
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

/// `[<kind>.<name>.generate]` — ソースを作る
/// （[ADR-0054](../../../docs/adr/0054-generated-sources.md)）。
///
/// 生成器は `sh` の小さな台本にする。bison も protoc も持ち込まずに、
/// 「作られたものが翻訳へ渡る」経路そのものを通せる。
#[test]
fn a_target_compiles_the_sources_it_generates() {
    if !program_exists("sh") {
        eprintln!("skipping: sh is not on PATH");
        return;
    }
    let p = Project::new("generate-sources");
    p.write("dowel.toml", "[package]\nname = \"calc\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.calc]\nsources = glob(\"src/*.c\")\n\n\
         [bin.calc.generate]\n\
         table = { command = \"sh\", args = [file(\"gen.sh\")], \
         inputs = [file(\"src/table.txt\")], outputs = [\"table.c\", \"table.h\"] }\n",
    );
    // 作業ディレクトリは出力の置き場所である。台本は相対名で書けば足りる。
    p.write(
        "gen.sh",
        "set -eu\nn=$(cat \"$1\")\n\
         printf 'int table_size(void);\\n' > table.h\n\
         printf '#include \"table.h\"\\nint table_size(void) { return %s; }\\n' \"$n\" > table.c\n",
    );
    p.write("src/table.txt", "7\n");
    p.write(
        "src/main.c",
        "#include <stdio.h>\n#include \"table.h\"\n\
         int main(void) { printf(\"n=%d\\n\", table_size()); return 0; }\n",
    );

    let r = p.run(".", &["build"]);
    r.success();
    let dir = build_dir(&p.path("."), "debug");
    assert!(dir.join("generated/calc/calc/table/table.c").exists(), "{r}");
    assert_eq!(run_artifact(&dir.join("bin/calc")), "n=7\n");

    // 読むものが変われば作り直し、それを読んだ翻訳も走り直す。
    p.write("src/table.txt", "9\n");
    p.run(".", &["build"]).success();
    assert_eq!(run_artifact(&dir.join("bin/calc")), "n=9\n");

    // 変わらなければ何もしない。生成の出力を毎回作り直すと、それを入力に
    // 持つ翻訳も毎回走る——増分ビルドがターゲットごと成り立たなくなる。
    let r = p.run(".", &["build", "--backend=direct", "--log-level=debug"]);
    r.success();
    r.stderr_contains("ran 0 steps");
}

/// 生成された頭部は、それを読みうる翻訳より先に作られなければならない。
///
/// ninja が読むのは入力と出力の関係であり、計画の持つ辺ではない。頭部を
/// 翻訳の入力に置かなければ、順序は言われていないのと同じである（ADR-0054）。
#[test]
fn a_generated_header_is_made_before_the_compiles_that_may_read_it() {
    if !program_exists("sh") {
        eprintln!("skipping: sh is not on PATH");
        return;
    }
    let p = Project::new("generate-header-only");
    p.write("dowel.toml", "[package]\nname = \"app\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.generate]\n\
         limits = { command = \"sh\", args = [file(\"gen.sh\")], \
         inputs = [file(\"src/limit.txt\")], outputs = [\"limits.h\"] }\n",
    );
    p.write("gen.sh", "set -eu\nprintf '#define LIMIT %s\\n' \"$(cat \"$1\")\" > limits.h\n");
    p.write("src/limit.txt", "5\n");
    p.write(
        "src/main.c",
        "#include <stdio.h>\n#include \"limits.h\"\n\
         int main(void) { printf(\"%d\\n\", LIMIT); return 0; }\n",
    );

    p.run(".", &["build"]).success();
    let dir = build_dir(&p.path("."), "debug");
    assert_eq!(run_artifact(&dir.join("bin/app")), "5\n");
}

/// 出力の場所が依存側へ届くのは `public` を宣言したときだけである。
/// 届き方は `public.includes` と同じ（ADR-0054）。
#[test]
fn a_generated_directory_reaches_dependents_only_when_it_is_public() {
    if !program_exists("sh") {
        eprintln!("skipping: sh is not on PATH");
        return;
    }
    let p = Project::new("generate-public");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"lib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"limits.h\"\nint lib_limit(void);\n\
         int main(void) { printf(\"%d %d\\n\", LIMIT, lib_limit()); return 0; }\n",
    );
    p.write("lib/dowel.toml", "[package]\nname = \"lib\"\nversion = \"0\"\n");
    p.write("lib/gen.sh", "set -eu\nprintf '#define LIMIT %s\\n' \"$(cat \"$1\")\" > limits.h\n");
    p.write("lib/src/limit.txt", "5\n");
    p.write("lib/src/lib.c", "#include \"limits.h\"\nint lib_limit(void) { return LIMIT; }\n");

    let declaration = |public: &str| {
        format!(
            "[lib.lib]\nsources = glob(\"src/*.c\")\n\n\
             [lib.lib.generate]\n\
             limits = {{ command = \"sh\", args = [file(\"gen.sh\")], \
             inputs = [file(\"src/limit.txt\")], outputs = [\"limits.h\"]{public} }}\n"
        )
    };

    p.write("lib/dowel.build", &declaration(""));
    // 依存側は「そんな頭部は無い」で落ちる。届いていないことの証拠である。
    p.run("app", &["build"]).failure();

    p.write("lib/dowel.build", &declaration(", public = true"));
    p.run("app", &["build"]).success();
    assert_eq!(run_artifact(&build_dir(&p.path("app"), "debug").join("bin/app")), "5 5\n");
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
    // 第1引数で振る舞いを変える。`fail` は非零、`hang` は終わらない、
    // `crash` は落ちる、`cwd` は走っている場所を書き出す。
    p.write(
        "tests/suite.c",
        r#"#include <stdio.h>
#include <string.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    const char *what = argc > 1 ? argv[1] : "ok";
    if (strcmp(what, "fail") == 0) { return 3; }
    if (strcmp(what, "hang") == 0) { for (;;) { } }
    if (strcmp(what, "crash") == 0) { *(volatile int *)0 = 1; }
    if (strcmp(what, "cwd") == 0) {
        FILE *f = fopen("here.txt", "r");
        if (!f) { printf("no here.txt\n"); return 1; }
        fclose(f);
        return 0;
    }
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

    // 誰も名乗っていない名前は、黙って0件成功にしない。「綴りを間違えた」と
    // 「1件通った」が呼び出し側から同じに見えてはならない（issue #89）。
    let r = p.run(".", &["test", "--label=nosuch"]);
    r.failure();
    r.stderr_contains("no test carries `nosuch`");
    r.stderr_contains("--no-run");
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
    // 目標と事例は別の欄である。読む側が最後の `/` で割らずに済む（issue #100）。
    r.stdout_contains("\"target\":\"suite:suite\"");
    r.stdout_contains("\"case\":\"slow\"");
    r.stdout_contains("\"label\":\"suite:suite/slow\"");
    r.stdout_contains("\"timed_out\":true");
    r.stdout_contains("\"timeout\":1");
    // 時間切れで殺したのはこちらである。プログラムの終わり方ではない。
    r.stdout_contains("\"signal\":null");
}

#[test]
fn a_case_killed_by_a_signal_does_not_satisfy_should_fail() {
    // `should_fail` を書く場所は「壊れた入力を食わせる事例」であり、
    // それは落ちやすい事例でもある。落ちたことを期待どおりとすると、
    // 最も捕まえたい欠陥が緑になる（issue #88）。
    let p = case_project("cases-crash", "rejects = { args = [\"crash\"], should_fail = true }\n");
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("suite:suite/rejects ... FAILED");
    r.stderr_contains("killed by signal 11 (SIGSEGV)");
    r.stderr_contains("not a crash");
}

#[test]
fn the_machine_readable_result_separates_a_crash_from_a_nonzero_exit() {
    // `exit_status: null` は時間切れでもシグナルでも起きる。下流が判定
    // できるように、それぞれの欄を持たせる（issue #88 / #100）。
    let p = case_project(
        "cases-crash-json",
        "rejects = { args = [\"fail\"], should_fail = true }\ncrashes = { args = [\"crash\"] }\n",
    );
    let r = p.run(".", &["test", "--message-format=json"]);
    r.failure();
    // 期待された失敗。状態3で終わったことも、期待していたことも読める。
    r.stdout_contains(
        "\"case\":\"rejects\",\"label\":\"suite:suite/rejects\",\"labels\":[],\"should_fail\":true",
    );
    r.stdout_contains("\"exit_status\":3,\"signal\":null");
    // 落ちた方。状態は無く、シグナルがある。
    r.stdout_contains("\"exit_status\":null,\"signal\":11");
}

#[test]
fn a_case_can_be_given_the_directory_it_runs_in() {
    // 資料を相対パスで読むテストは、どこから走るかを決められなければ書けない
    // （issue #95）。
    let p = case_project(
        "cases-cwd",
        "here = { args = [\"cwd\"], cwd = dir(\"tests/golden\") }\nroot = { args = [\"cwd\"] }\n",
    );
    p.write("tests/golden/here.txt", "x\n");
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("suite:suite/here ... ok");
    // 既定はパッケージの根。そこに `here.txt` は無い。
    r.stderr_contains("suite:suite/root ... FAILED");
    r.stderr_contains("no here.txt");
}

#[test]
fn a_case_whose_directory_does_not_exist_says_so() {
    // `spawn` の `No such file or directory` は、実行ファイルが無いようにも
    // 読める。どちらが無いのかを述べる。
    let p = case_project("cases-cwd-missing", "one = { cwd = dir(\"nosuch\") }\n");
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("the working directory does not exist");
    r.stderr_contains("nosuch");
}

#[test]
fn the_schema_dump_describes_the_properties_a_case_accepts() {
    // 文書と型検査器とダンプが同じ表を読む、という約束が破れていた
    // （issue #90）。`cases` だけが抜けていた。
    let p = Project::new("schema-cases");
    let r = p.run(".", &["schema", "dump"]);
    r.success();
    r.stdout_contains("\"case_properties\"");
    r.stdout_contains("\"name\": \"should_fail\"");
    r.stdout_contains("\"harness_properties\"");
    // ランナーの鍵表も同じく出ていなかった。
    r.stdout_contains("\"runner_properties\"");
    r.stdout_contains("\"name\": \"remote_dir\"");
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
    r.stderr_contains("only `test` and `bench` targets register cases");
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
    /* ラベルの文法を壊す名前を返す枠組み。既存のフレームワークの出力には
       空白も `/` も普通に現れる。 */
    if (argc > 1 && strcmp(argv[1], "--list-odd") == 0) {
        printf("alpha\na/b\n");
        return 0;
    }
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
    // 名乗っていない名前では1件も選ばれず、状態も非零になる。
    let r = p.run(".", &["test", "--label=slow"]);
    r.failure();
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
fn the_stub_arguments_do_not_break_a_runner_that_ends_with_the_artifact_flag() {
    // `args` の末尾が「成果物を取るフラグ」である runner——ADR-0008 が勧める
    // 形そのもの——に、スタブの引数を後ろから挿すと、フラグがそれを成果物
    // として食う（issue #107）。qemu-user の `-g` は位置に依存しないので、
    // これは qemu-system で初めて現れる。
    let p = debug_project(
        "debug-kernel-flag",
        "\n[toolchain.thumbv7em-none-eabihf]\nc = \"cc\"\n",
        "\n[runner.thumbv7em-none-eabihf]\ncommand       = \"qemu-system-arm\"\nargs          = [\"-M\", \"mps2-an386\", \"-nographic\", \"-kernel\"]\ndebug_args    = [\"-gdb\", \"tcp::13579\", \"-S\"]\ndebug_connect = \"localhost:13579\"\n",
    );
    let r = p.run(".", &["debug", "app", "--target=thumbv7em-none-eabihf", "--dap"]);
    r.success();
    let args = &r.stdout[r.stdout.find("debugServerArgs").expect("the stub args are missing")..];
    let args = &args[..args.find(']').expect("the stub args never end")];
    // `-kernel` の次は成果物でなければならない。隣接の対を割ってはいけない。
    let kernel = args.find("\"-kernel\"").expect("`-kernel` is not among the stub args");
    let artifact = args.find("bin/app\"").expect("the artifact is not among the stub args");
    assert!(kernel < artifact, "the artifact came before `-kernel`\n{r}");
    let between = &args[kernel..artifact];
    assert!(
        !between.contains("-gdb") && !between.contains("-S"),
        "the stub arguments were inserted between `-kernel` and the artifact\n{r}"
    );
    // スタブの引数は runner の引数より前に来る。
    let gdb = args.find("\"-gdb\"").expect("`-gdb` is not among the stub args");
    assert!(gdb < kernel, "the stub arguments came after the runner's own\n{r}");
}

#[test]
fn a_half_declared_stub_is_told_which_half_is_missing() {
    // 「両方無い」と「片方だけ」を同じ文言にしない。半分書いた利用者に
    // 「宣言が無い」と言うと、書いてある側を見返させることになる（issue #109）。
    let no_address = debug_project(
        "debug-half-args",
        "\n[toolchain.aarch64-unknown-linux-gnu]\nc = \"cc\"\n",
        "\n[runner.aarch64-unknown-linux-gnu]\ncommand    = \"qemu-aarch64\"\ndebug_args = [\"-g\", \"12345\"]\n",
    );
    let r = no_address.run(".", &["debug", "app", "--target=aarch64-unknown-linux-gnu"]);
    r.failure();
    r.stderr_contains("missing-debug-stub");
    r.stderr_contains("has no attach address");
    r.stderr_contains("the host side is declared");
    r.stderr_contains("debug_connect");
    assert!(!r.stderr.contains("declares no stub"), "a half-declared stub was called empty\n{r}");

    let no_host = debug_project(
        "debug-half-connect",
        "\n[toolchain.aarch64-unknown-linux-gnu]\nc = \"cc\"\n",
        "\n[runner.aarch64-unknown-linux-gnu]\ncommand       = \"qemu-aarch64\"\ndebug_connect = \"localhost:12345\"\n",
    );
    let r = no_host.run(".", &["debug", "app", "--target=aarch64-unknown-linux-gnu"]);
    r.failure();
    r.stderr_contains("nothing hosts the program");
    r.stderr_contains("the address to attach to is declared");
    r.stderr_contains("debug_args");
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

#[test]
fn two_targets_in_one_package_may_not_share_a_name() {
    // ライブラリ `wt` とその CLI `wt` は自然な書き方だが、名前は
    // `target("...")`・`<パッケージ>:<ターゲット>` のラベル・`obj/` の経路の
    // 3か所すべてが鍵にしている（issue #114）。
    let p = Project::new("dup-target");
    p.write("dowel.toml", "[package]\nname    = \"rep\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        "[lib.foo]\nsources = [file(\"src/foo.c\")]\n\n[lib.foo.public]\nincludes = [dir(\"include\")]\n\n[bin.foo]\nsources = [file(\"src/main.c\")]\n\n[bin.foo.private]\ndeps = [target(\"foo\")]\n",
    );
    p.write("include/h.h", "int foo(void);\n");
    p.write("src/foo.c", "#include \"h.h\"\nint foo(void) { return 0; }\n");
    p.write("src/main.c", "#include \"h.h\"\nint main(void) { return foo(); }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("duplicate-target");
    r.stderr_contains("`foo` is already a lib target");
    r.stderr_contains("declared here first");
    // 1つの誤りに1つの診断。ブロックの表ごとには出さない。
    assert_eq!(r.stderr.matches("duplicate-target").count(), 1, "the diagnostic repeated\n{r}");
}

#[test]
fn splitting_the_name_makes_the_same_tree_build() {
    // 対照。差はターゲット名だけであり、名前を割れば `public` は届き、
    // 共有されたソースも別々の `obj/` へ落ちる。
    let p = Project::new("dup-target-split");
    p.write("dowel.toml", "[package]\nname    = \"rep\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        "[lib.foolib]\nsources = [file(\"src/shared.c\")]\n\n[lib.foolib.public]\nincludes = [dir(\"include\")]\n\n[bin.foo]\nsources = [file(\"src/shared.c\"), file(\"src/main.c\")]\n\n[bin.foo.private]\ndeps = [target(\"foolib\")]\n",
    );
    p.write("include/h.h", "int shared(void);\n");
    p.write("src/shared.c", "#include \"h.h\"\nint shared(void) { return 0; }\n");
    p.write("src/main.c", "#include \"h.h\"\nint main(void) { return shared(); }\n");
    let r = p.run(".", &["build", "foo", "foolib"]);
    r.success();
    // 同じソースを両方が持っても、経路は種別ではなく名前で分かれている。
    let dir = build_dir(&p.path("."), "debug");
    assert!(dir.join("lib/libfoolib.a").exists(), "the archive is missing\n{r}");
    assert!(dir.join("bin/foo").exists(), "the executable is missing\n{r}");
}

/// 対象の OS と構成（[ADR-0026](../../../docs/adr/0026-target-os-arch.md)、
/// issue #115 / #112）。
///
/// 三つ組は `--target` で自由に指定できるので、ホストの `cc` を Windows の
/// 三つ組に宣言すれば、対象の綴りの規則だけを取り出して検査できる。
/// 出来上がるのは Linux の実行ファイルだが、dowel が名指しする道・runner に
/// 渡る道・指紋が取る道はすべて対象の規則に従う。
fn target_os_project(name: &str) -> Project {
    let p = Project::new(name);
    // ドライバが**勝手に `.exe` を付ける**ことが問題の根なので、それを
    // 再現する偽のドライバを置く。ホストの `cc` をそのまま宣言しても
    // 症状は出ない——出来上がるのは Linux の実行ファイルだが、綴りの規則の
    // 検査にはそれで足りる。
    let driver = p.write_script(
        "mingw-ish",
        r#"#!/bin/sh
# リンクのときだけ、出力に `.exe` を付ける（既に付いていれば足さない）。
out=""; link=1; args=""
while [ $# -gt 0 ]; do
    case "$1" in
        -c) link=0; args="$args $1" ;;
        -o) out="$2"; shift; args="$args -o" ;;
        *)  args="$args $1" ;;
    esac
    shift
done
if [ "$link" = 1 ] && [ -n "$out" ]; then
    case "$out" in *.exe) ;; *) out="$out.exe" ;; esac
fi
# shellcheck disable=SC2086
exec cc $args "$out"
"#,
    );
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"w\"\nversion = \"0.1.0\"\n\n[toolchain.x86_64-pc-windows-gnu]\nc = \"{}\"\n",
            driver.display()
        ),
    );
    p.write(
        "dowel.build",
        r#"
[bin.app]
sources = [file("src/main.c"), match target.os {
    windows => file("src/plat_win.c"),
    _       => file("src/plat_posix.c"),
}]

[runner.x86_64-pc-windows-gnu]
command = "env"
"#,
    );
    p.write("src/main.c", "int plat(void);\nint main(void) { return plat(); }\n");
    p.write("src/plat_win.c", "int plat(void) { return 0; }\n");
    p.write("src/plat_posix.c", "int plat(void) { return 3; }\n");
    p
}

#[test]
fn a_manifest_can_select_sources_by_the_targets_operating_system() {
    // `host.os` は組む側を指すので、素直に書くと意図と逆に効いていた
    // （issue #115）。`target.os` は `--target` の三つ組から導かれる。
    let p = target_os_project("target-os-sources");
    let cross =
        p.run(".", &["graph", "--kind=action", "--format=json", "--target=x86_64-pc-windows-gnu"]);
    cross.success();
    cross.stdout_contains("plat_win.c");
    assert!(
        !cross.stdout.contains("plat_posix.c"),
        "the POSIX file was chosen for windows\n{cross}"
    );

    // 対照。同じ木を手元向けに組めば POSIX 側が選ばれる。
    let host = p.run(".", &["graph", "--kind=action", "--format=json"]);
    host.success();
    host.stdout_contains("plat_posix.c");
}

#[test]
fn the_artifact_dowel_names_is_the_file_that_was_written() {
    // Windows 対象ではドライバが `.exe` を付けて書く。dowel が名指しする道と
    // 実在するファイルがずれると、組む段では現れず、走らせる・派生させる・
    // 開くの全部が落ちる（issue #112）。
    let p = target_os_project("target-os-exe");
    let r = p.run(".", &["build", "--target=x86_64-pc-windows-gnu"]);
    r.success();
    let named = r
        .stderr
        .lines()
        .chain(r.stdout.lines())
        .find_map(|l| l.strip_prefix("built: "))
        .expect("the build printed no artifact");
    assert!(named.ends_with("bin/app.exe"), "dowel named `{named}`\n{r}");
    assert!(std::path::Path::new(named).exists(), "`{named}` does not exist\n{r}");
}

#[test]
fn a_second_windows_build_runs_nothing() {
    // 宣言した出力が永久に存在しないと、「出力が無い」と「まだ作っていない」が
    // 同じ状態に潰れ、増分が収束しない。成功したように見えるので、気づく
    // 手がかりは所要時間だけだった（issue #112 のコメント）。
    let p = target_os_project("target-os-incremental");
    let args =
        &["build", "--target=x86_64-pc-windows-gnu", "--backend=direct", "--log-level=debug"];
    p.run(".", args).success();
    let second = p.run(".", args);
    second.success();
    second.stderr_contains("ran 0 steps");
}

#[test]
fn a_windows_target_can_be_tested_through_its_runner() {
    // runner に渡る道も同じ規則から来る。`.exe` の無い道を渡すと、
    // wine は `failed to open` で落ちる。
    let p = target_os_project("target-os-runner");
    p.write(
        "dowel.build",
        "[test.t]\nsources = [file(\"src/t.c\")]\n\n[runner.x86_64-pc-windows-gnu]\ncommand = \"env\"\n",
    );
    p.write("src/t.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["test", "--target=x86_64-pc-windows-gnu"]);
    r.success();
    r.stderr_contains("w:t ... ok");
}

#[test]
fn a_derived_target_property_is_exhaustively_checked() {
    // 有限領域であることが、三つ組を数え上げる形との差である。`_` を書かずに
    // 済み、対象が増えたときにマニフェストが落ちて教える。
    let p = Project::new("target-os-exhaustive");
    p.write("dowel.toml", "[package]\nname    = \"w\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\nflags = match target.os {\n    linux   => [\"-DL\"],\n    windows => [\"-DW\"],\n}\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("non-exhaustive-match");
    // 何が漏れているかを名指しする。
    r.stderr_contains("macos");
    r.stderr_contains("none");
    r.stderr_contains("other");
}

#[test]
fn the_target_vocabulary_is_in_the_schema_dump() {
    let p = Project::new("target-os-schema");
    let r = p.run(".", &["schema", "dump"]);
    r.success();
    r.stdout_contains("\"name\": \"target.os\"");
    r.stdout_contains("\"name\": \"target.arch\"");
    // 値域が出る。`_` を書かずに済むことが読める。
    r.stdout_contains("\"none\"");
    // `host.*` は残る。
    r.stdout_contains("\"name\": \"host.os\"");

    // 語彙が閉じていることは、機械の側にも要る情報である。「暫定」と
    // 言われた語彙を当てにする理由は無い（ADR-0034、issue #143）。
    r.stdout_contains("\"status\": \"closed;");
    assert!(
        !r.stdout.contains("provisional"),
        "the schema still calls it provisional:\n{}",
        r.stdout
    );

    // 人が読む側と食い違わない。診断も閉じていると述べている。
    p.write("dowel.toml", "[package]\nname = \"x\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\nprivate.flags = [\"-x\" when cfg.sanitizer]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("the vocabulary is closed");
}

/// MSVC の様式（[ADR-0027](../../../docs/adr/0027-toolchain-style.md)、
/// issue #113）。
///
/// MSVC が無い機械では「組めるか」は問えないが、「宣言できるか」は問える——
/// argv を記録するだけの偽の `cl` を置き、組み上がる命令の形を読む。
/// 報告と同じ見方である。
fn msvc_project(name: &str) -> Project {
    let p = Project::new(name);
    // 何もせず終わるだけの道具。実在検査を通すために要る。
    for tool in ["cl", "lib", "link"] {
        p.write_script(&format!("fake/{tool}"), "#!/bin/sh\nexit 0\n");
    }
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"rep\"\nversion = \"0.1.0\"\n\n[toolchain.x86_64-pc-windows-msvc]\nc   = \"{d}/cl\"\ncxx = \"{d}/cl\"\nar  = \"{d}/lib\"\nlink = \"{d}/link\"\n",
            d = p.path("fake").display()
        ),
    );
    p.write(
        "dowel.build",
        "[lib.core]\nsources = [file(\"src/core.c\")]\n\n[lib.core.public]\nincludes = [dir(\"include\")]\ndefines  = { CORE = 1 }\n\n[bin.app]\nsources = [file(\"src/main.c\")]\n\n[bin.app.private]\ndeps = [target(\"core\")]\n",
    );
    p.write("include/h.h", "int core(void);\n");
    p.write("src/core.c", "int core(void) { return 0; }\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");
    p
}

#[test]
fn an_msvc_toolchain_can_be_declared_not_just_named() {
    // 名前だけ宣言できても、綴りが Unix 固定なら cl が解釈できない命令が
    // 組み上がる（issue #113）。
    let p = msvc_project("msvc-style");
    let r =
        p.run(".", &["graph", "--kind=action", "--format=json", "--target=x86_64-pc-windows-msvc"]);
    r.success();
    let out = &r.stdout;
    // 翻訳。`/I` `/D` `/c` `/Fo:` であり、`-I` `-o` ではない。
    assert!(out.contains("\"/DCORE=1\""), "{r}");
    assert!(out.contains("/Fo:"), "{r}");
    assert!(out.contains("\"/c\""), "{r}");
    assert!(out.contains("\"/Od\""), "{r}");
    // 書庫は `lib /OUT:core.lib`。`rcs` でも `libcore.a` でもない。
    assert!(out.contains("/OUT:"), "{r}");
    assert!(out.contains("core.lib"), "{r}");
    assert!(!out.contains("libcore.a"), "{r}");
    // オブジェクトは `.obj`。
    assert!(out.contains(".c.obj"), "{r}");
    // 実行ファイルは `.exe`（ADR-0026 の導出がここでも効く）。
    assert!(out.contains("app.exe"), "{r}");
    // GNU の綴りは1つも混ざらない。
    for gnu in ["\"-I", "\"-D", "\"-c\"", "\"-o\"", "\"-g\"", "\"rcs\""] {
        assert!(!out.contains(gnu), "the GNU spelling `{gnu}` survived\n{r}");
    }
}

#[test]
fn the_dependency_flag_that_means_something_else_is_never_emitted() {
    // `-MD` は MSVC で「動的 CRT をリンクする」を意味する。依存の書き出しを
    // 頼んだつもりの旗が ABI を選ぶ旗になる——overview が「No single ABI」の
    // 例として挙げているまさにその旗である（issue #113）。
    let p = msvc_project("msvc-md");
    let r =
        p.run(".", &["graph", "--kind=action", "--format=json", "--target=x86_64-pc-windows-msvc"]);
    r.success();
    assert!(!r.stdout.contains("\"-MD\""), "`-MD` reached an MSVC command line\n{r}");
    assert!(
        !r.stdout.contains("\"/MD\""),
        "`/MD` was emitted as if it were a dependency flag\n{r}"
    );
    assert!(!r.stdout.contains("\"-MF\""), "{r}");
    // 依存は別の機構で取る。
    r.stdout_contains("\"/showIncludes\"");
    r.stdout_contains("\"deps\": \"show-includes\"");
}

#[test]
fn the_style_follows_the_triple_and_a_declaration_overrides_it() {
    // 三つ組が様式を決める（ADR-0026 と同じ判断）。`style` はその上書き。
    let p = msvc_project("msvc-style-decl");
    let graph = ["graph", "--kind=action", "--format=json"];

    // 導出。MinGW の三つ組は GNU、`-msvc` の三つ組は MSVC。宣言は無い。
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"rep\"\nversion = \"0.1.0\"\n\n[toolchain.x86_64-pc-windows-gnu]\nc = \"cc\"\nar = \"ar\"\n\n[toolchain.x86_64-pc-windows-msvc]\nc   = \"{d}/cl\"\ncxx = \"{d}/cl\"\nar  = \"{d}/lib\"\nlink = \"{d}/link\"\n",
            d = p.path("fake").display()
        ),
    );
    let mingw = p.run(".", &[&graph[..], &["--target=x86_64-pc-windows-gnu"]].concat());
    mingw.success();
    mingw.stdout_contains("\"-c\"");
    mingw.stdout_contains("libcore.a");
    // `.exe` は様式ではなく OS が決めるので、GNU の側にも付く。
    mingw.stdout_contains("app.exe");

    let msvc = p.run(".", &[&graph[..], &["--target=x86_64-pc-windows-msvc"]].concat());
    msvc.success();
    msvc.stdout_contains("/Fo:");
    msvc.stdout_contains("core.lib");
    assert!(!msvc.stdout.contains("\"-c\""), "the triple did not select the MSVC style\n{msvc}");

    // 宣言は導出に勝つ。導出と**逆向き**に書く——同じ向きだと、導出が
    // 壊れていても宣言が効いているように見える。
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"rep\"\nversion = \"0.1.0\"\n\n[toolchain.x86_64-pc-windows-gnu]\nstyle = \"msvc\"\nc   = \"{d}/cl\"\ncxx = \"{d}/cl\"\nar  = \"{d}/lib\"\nlink = \"{d}/link\"\n",
            d = p.path("fake").display()
        ),
    );
    let overridden = p.run(".", &[&graph[..], &["--target=x86_64-pc-windows-gnu"]].concat());
    overridden.success();
    overridden.stdout_contains("/Fo:");
    overridden.stdout_contains("core.lib");
    assert!(
        !overridden.stdout.contains("\"-c\""),
        "the declaration did not override the derivation\n{overridden}"
    );
}

#[test]
fn an_unknown_style_is_refused_with_the_ones_that_exist() {
    let p = msvc_project("msvc-style-typo");
    p.write(
        "dowel.toml",
        "[package]\nname    = \"rep\"\nversion = \"0.1.0\"\n\n[toolchain]\nstyle = \"msvcc\"\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("invalid-value");
    r.stderr_contains("the styles are: gnu, msvc");
    r.stderr_contains("did you mean `msvc`?");
}

#[test]
fn the_flags_a_user_writes_are_not_translated() {
    // 綴りを翻訳しようとすると、旗の対応表を持つことになる。それは
    // 「コンパイラを知っている」ことに他ならない（ADR-0027）。
    let p = msvc_project("msvc-user-flags");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n[bin.app.private]\nflags = [\"/W4\", \"/permissive-\"]\nlink_flags = [\"ws2_32.lib\"]\n",
    );
    let r =
        p.run(".", &["graph", "--kind=action", "--format=json", "--target=x86_64-pc-windows-msvc"]);
    r.success();
    r.stdout_contains("\"/W4\"");
    r.stdout_contains("\"/permissive-\"");
    r.stdout_contains("\"ws2_32.lib\"");
}

#[test]
fn show_includes_output_becomes_the_dependency_record() {
    // MSVC はヘッダ依存の記録を書かない。標準出力に並べるだけなので、
    // 畳むのは実行した側の仕事になる（ADR-0027）。畳めていなければ、
    // ヘッダを触っても再翻訳されない。
    let p = Project::new("msvc-showincludes");
    // `/showIncludes` を出しつつ実際に翻訳する偽の `cl`。GNU の綴りへ
    // 読み替えて `cc` に渡す——検査したいのは依存の記録の経路である。
    p.write_script(
        "fake/cl",
        r#"#!/bin/sh
args=""; out=""; src=""
while [ $# -gt 0 ]; do
    case "$1" in
        /nologo|/showIncludes|/Z7|/Od|/O2) ;;
        /c) args="$args -c" ;;
        /Fo:*) out="${1#/Fo:}" ;;
        /I*) args="$args -I${1#/I}" ;;
        /D*) args="$args -D${1#/D}" ;;
        *) src="$1" ;;
    esac
    shift
done
# ヘッダの依存を、cl と同じ形で並べる。
for h in $(sed -n 's/^#include "\(.*\)"/\1/p' "$src"); do
    d=$(dirname "$src")
    echo "Note: including file: $d/$h"
done
# shellcheck disable=SC2086
exec cc $args "$src" -o "$out"
"#,
    );
    // リンクも綴りが違う。`/OUT:` を読み替えるだけの偽の `link`。
    p.write_script(
        "fake/link",
        r#"#!/bin/sh
args=""; out=""
while [ $# -gt 0 ]; do
    case "$1" in
        /nologo) ;;
        /OUT:*) out="${1#/OUT:}" ;;
        *) args="$args $1" ;;
    esac
    shift
done
# shellcheck disable=SC2086
exec cc $args -o "$out"
"#,
    );
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"rep\"\nversion = \"0.1.0\"\n\n[toolchain.x86_64-pc-windows-msvc]\nc = \"{d}/cl\"\nlink = \"{d}/link\"\n",
            d = p.path("fake").display()
        ),
    );
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/h.h", "#define V 0\n");
    p.write("src/main.c", "#include \"h.h\"\nint main(void) { return V; }\n");

    let args =
        &["build", "--target=x86_64-pc-windows-msvc", "--backend=direct", "--log-level=debug"];
    p.run(".", args).success();
    // 記録が `.d` に畳まれている。読む側は様式を知らずに済む。
    let dir = build_dir(&p.path("."), "x86_64-pc-windows-msvc-debug");
    let dep = dir.join("obj/rep/app/src_main.c.obj.d");
    let text = std::fs::read_to_string(&dep).expect("no dependency record was written");
    assert!(text.contains("h.h"), "the record does not name the header: {text}");

    // 2度目は何もしない。
    let second = p.run(".", args);
    second.success();
    second.stderr_contains("ran 0 steps");

    // ヘッダを触ると翻訳し直す——記録が効いている証拠である。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    p.write("src/h.h", "#define V 0 /* touched */\n");
    let third = p.run(".", args);
    third.success();
    assert!(!third.stderr.contains("ran 0 steps"), "a header change was missed\n{third}");
}

/// 道具について確かめた事実（[ADR-0028](../../../docs/adr/0028-probe-facts.md)）。
///
/// 事実はプロジェクトの外（利用者のキャッシュ領域）に置かれる。検査は
/// `XDG_CACHE_HOME` を木の中へ向けて、利用者の環境を汚さずに読む。
fn facts_project(name: &str) -> (Project, String) {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"d\"\nversion = \"0.1.0\"\n");
    p.write("dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");
    let cache = p.path("cache").display().to_string();
    (p, cache)
}

#[test]
fn what_was_asked_of_a_tool_is_not_asked_again() {
    let (p, cache) = facts_project("facts-reuse");
    let env: &[(&str, &str)] = &[("XDG_CACHE_HOME", &cache), ("DOWEL_LOG", "debug")];

    let first = p.run_env(".", &["build"], env);
    first.success();
    // 1回目は道具に訊く。何を訊くかは構成次第なので、数は問わない。
    let launched = |r: &common::Run| {
        r.stderr
            .lines()
            .find_map(|l| {
                l.split("probe: launched ").nth(1)?.split(' ').next()?.parse::<u32>().ok()
            })
            .expect("the probe count is missing from the log")
    };
    assert!(launched(&first) > 0, "nothing was probed on the first run\n{first}");

    // 2回目は憶えている。プロセスを1つも起こさない。
    let second = p.run_env(".", &["build"], env);
    second.success();
    assert_eq!(launched(&second), 0, "a remembered fact was asked again\n{second}");
}

#[test]
fn the_facts_live_outside_the_project() {
    // 事実は道具のものであってプロジェクトのものではない。木を消しても
    // 残り、別の木から引ける。
    let (p, cache) = facts_project("facts-outside");
    let env: &[(&str, &str)] = &[("XDG_CACHE_HOME", &cache)];
    p.run_env(".", &["build"], env).success();

    let facts = std::path::Path::new(&cache).join("dowel/facts/v1/facts");
    assert!(facts.exists(), "no fact file at {}", facts.display());
    // ビルドディレクトリの中には無い。
    let build = p.path(".dowel");
    assert!(!build.join("facts").exists(), "facts were written into the project");

    // 別の木が同じ事実を引く。
    let (other, _) = facts_project("facts-outside-second");
    let second =
        other.run_env(".", &["build"], &[("XDG_CACHE_HOME", &cache), ("DOWEL_LOG", "debug")]);
    second.success();
    second.stderr_contains("probe: launched 0 process(es)");
}

#[test]
fn the_host_triple_is_what_the_compiler_calls_itself() {
    // dowel が組み立てる綴りは近似である。道具が別の名を名乗る機械で、
    // その名を `--target` に渡した利用者がクロス扱いされてはならない
    // （ADR-0028）。
    let (p, cache) = facts_project("facts-host-triple");
    // 三つ組を名乗る偽のコンパイラ。実際の翻訳は cc に委ねる。
    let cc = p.write_script(
        "fake/cc",
        r#"#!/bin/sh
if [ "$1" = "-dumpmachine" ]; then echo custom-vendor-linux-gnu; exit 0; fi
exec cc "$@"
"#,
    );
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname    = \"d\"\nversion = \"0.1.0\"\n\n[toolchain]\nc = \"{}\"\n",
            cc.display()
        ),
    );
    p.write("dowel.build", "[test.t]\nsources = glob(\"src/*.c\")\n");
    let env: &[(&str, &str)] = &[("XDG_CACHE_HOME", &cache)];

    // 名乗った綴りはホストとして通る。ランナーは要らない。
    let named = p.run_env(".", &["test", "--target=custom-vendor-linux-gnu"], env);
    named.success();
    named.stderr_contains("d:t ... ok");

    // dowel の近似もホストのままである。片方に寄せると、もう片方が
    // クロス扱いになる。
    let approx = p.run_env(".", &["test"], env);
    approx.success();
    approx.stderr_contains("d:t ... ok");

    // 本当に別の機械を指す三つ組は、今までどおり宣言を求める。
    let cross = p.run_env(".", &["test", "--target=riscv64gc-unknown-linux-gnu"], env);
    cross.failure();
    cross.stderr_contains("missing-toolchain");
}

#[test]
fn the_cache_commands_cover_the_facts_too() {
    let (p, cache) = facts_project("facts-cache-cmd");
    let env: &[(&str, &str)] = &[("XDG_CACHE_HOME", &cache)];
    p.run_env(".", &["build"], env).success();

    let info = p.run_env(".", &["cache", "info"], env);
    info.success();
    info.stdout_contains("facts");
    // 消えたときに探す先が2つあることが読める。
    info.stdout_contains("dowel/facts/v1");

    // 古い形式版を置くと回収される。
    let stale = std::path::Path::new(&cache).join("dowel/facts/v0");
    std::fs::create_dir_all(&stale).unwrap();
    let gc = p.run_env(".", &["cache", "gc"], env);
    gc.success();
    gc.stderr_contains("fact database(s)");
    assert!(!stale.exists(), "the old format version survived gc");
}

/// `dowel bench`（ADR-0025）。
///
/// 測るのはプロセス全体の壁時計であり、枠組みは課さない。dowel が失敗と
/// 呼ぶのは走らせられなかったことだけで、速さに合否は無い。
fn bench_project(name: &str, extra: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"b\"\nversion = \"0.1.0\"\n");
    p.write("dowel.build", &format!("[bench.spin]\nsources = glob(\"bench/*.c\")\n{extra}"));
    p.write(
        "bench/spin.c",
        r#"#include <string.h>
int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "boom") == 0) { return 7; }
    volatile int x = 0;
    for (int i = 0; i < 1000; i++) { x += i; }
    return 0;
}
"#,
    );
    p
}

#[test]
fn a_bench_target_reports_min_and_median() {
    let p = bench_project("bench-basic", "");
    let r = p.run(".", &["bench", "--iterations=3"]);
    r.success();
    r.stderr_contains("measuring 1 benchmark");
    r.stderr_contains("bench b:spin ... min ");
    r.stderr_contains("median ");
    r.stderr_contains("(3 runs)");
}

#[test]
fn bench_cases_measure_the_same_binary_with_different_arguments() {
    // 事例の形はテストと同じ（ADR-0022 の再利用）。翻訳の単位は増えない。
    let p = bench_project(
        "bench-cases",
        "\n[bench.spin.cases]\nsmall = { args = [] }\nbig   = { args = [\"x\"] }\n",
    );
    let r = p.run(".", &["bench", "--iterations=2"]);
    r.success();
    r.stderr_contains("bench b:spin/small ... min ");
    r.stderr_contains("bench b:spin/big ... min ");
    // 事例の名指しも同じ形。
    let one = p.run(".", &["bench", "b:spin/big", "--iterations=2"]);
    one.success();
    one.stderr_contains("measuring 1 benchmark");
}

#[test]
fn a_bench_that_cannot_run_is_a_failure_without_numbers() {
    // 速さに合否は無いが、走らせられなかったことは失敗である。
    // 途中までの数字は「揃った計測」ではないので、出さない。
    let p = bench_project("bench-broken", "\n[bench.spin.cases]\nboom = { args = [\"boom\"] }\n");
    let r = p.run(".", &["bench", "--iterations=3"]);
    r.failure();
    r.stderr_contains("bench b:spin/boom ... FAILED");
    r.stderr_contains("run 1 exited with status 7");
    r.stderr_contains("could not be measured");
    assert!(!r.stderr.contains("median "), "a failed measurement reported numbers\n{r}");
}

#[test]
fn bench_results_are_machine_readable_in_microseconds() {
    let p = bench_project("bench-json", "");
    let r = p.run(".", &["bench", "--iterations=3", "--message-format=json"]);
    r.success();
    r.stdout_contains("\"kind\":\"bench-result\"");
    r.stdout_contains("\"target\":\"b:spin\"");
    r.stdout_contains("\"case\":null");
    r.stdout_contains("\"runs\":3");
    r.stdout_contains("\"min_us\":");
    r.stdout_contains("\"failure\":null");
}

#[test]
fn a_bench_case_does_not_take_should_fail() {
    // 計測に判定は無い。黙って無視すると「効いているように見えて効かない」。
    let p = bench_project(
        "bench-should-fail",
        "\n[bench.spin.cases]\nboom = { args = [\"boom\"], should_fail = true }\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-property");
    r.stderr_contains("a benchmark is measured, not judged");
}

#[test]
fn a_tree_without_bench_targets_says_so_and_succeeds() {
    let p = bench_project("bench-none", "");
    p.write("dowel.build", "[bin.app]\nsources = glob(\"bench/*.c\")\n");
    let r = p.run(".", &["bench"]);
    r.success();
    r.stderr_contains("no bench targets");
    // 名指しが種別違いなら断る。
    let wrong = p.run(".", &["bench", "app"]);
    wrong.failure();
    wrong.stderr_contains("not a bench");
}

#[test]
fn a_discovered_name_that_breaks_the_label_grammar_is_not_silently_accepted() {
    // マニフェストに書いた名前は #97 で検証されるようになった。同じ名前が
    // 列挙から来ると素通りしていた——規則が片方の入口にしか無い形である
    // （issue #108）。列挙が返す名前は書き手が選べないので、むしろこちらの
    // ほうが壊れた名前の来る確率は高い。
    let p = harness_project("harness-odd-names", "list = [\"--list-odd\"]\nrun = [\"--run\"]\n");
    let r = p.run(".", &["test"]);
    r.failure();
    r.stderr_contains("`a/b`");
    r.stderr_contains("cannot be a case name");
    r.stderr_contains("`/` separates the target from the case");
    // 列挙できなかった目標の失敗として出る。0件成功にはしない。
    r.stderr_contains("suite:suite ... FAILED");
    // 壊れたラベルで走らせない。
    assert!(!r.stderr.contains("suite:suite/a/b ... "), "the broken label still ran\n{r}");
}

/// `dowel debug <target>/<case>`（issue #110）。
#[test]
fn a_case_that_has_not_failed_can_be_opened_under_the_debugger() {
    // デバッガを開きたいのは失敗のときだけではない——通っているが遅い事例、
    // これから書く事例、別の構成で落ちた事例。記録を経由する道しか無いと、
    // 「わざと落として記録を作る」ことになる。
    let p = case_project(
        "debug-case-passing",
        "plain = { args = [\"env\"], env = { SUITE_MODE = \"strict\" }, cwd = dir(\"tests\") }\n",
    );
    // 一度も走らせていない（＝失敗の記録が無い）状態で開ける。
    let r = p.run(".", &["debug", "suite:suite/plain", "--dap"]);
    r.success();
    r.stderr_contains("debugging suite:suite/plain");
    r.stdout_contains("\"env\"");
    r.stdout_contains("\"name\": \"SUITE_MODE\"");
    r.stdout_contains("\"value\": \"strict\"");
    r.stdout_contains("tests\"");
}

#[test]
fn a_harness_case_can_be_opened_under_the_debugger_too() {
    // ハーネスが発見した事例も、`run` と名前を付けて開く。
    let p = harness_project("debug-case-harness", "list = [\"--list\"]\nrun = [\"--run\"]\n");
    let r = p.run(".", &["debug", "suite:suite/divides", "--dap"]);
    r.success();
    r.stderr_contains("debugging suite:suite/divides");
    r.stdout_contains("\"--run\"");
    r.stdout_contains("\"divides\"");
}

#[test]
fn naming_a_case_that_does_not_exist_says_which_ones_do() {
    let p = case_project("debug-case-unknown", "one = { args = [\"ok\"] }\n");
    let r = p.run(".", &["debug", "suite:suite/nosuch"]);
    r.failure();
    r.stderr_contains("no case named `suite:suite/nosuch`");
    r.stderr_contains("suite:suite/one");
}

#[test]
fn a_target_without_cases_says_so_when_one_is_named() {
    let p = debug_project("debug-case-on-bin", "", "");
    let r = p.run(".", &["debug", "app/nosuch"]);
    r.failure();
    r.stderr_contains("only `test` and `bench` targets have cases");
}

/// `dowel test --debug-failed`（docs/30-devexp.md 2.3）。
///
/// テストの仕事の列とデバッグの起動は既に揃っていた（ADR-0024）。ここは
/// 両者を繋ぐ——落ちた事例を、その宣言（引数・環境・作業ディレクトリ）の
/// ままデバッガの下で開き直す。
#[test]
fn the_failing_case_reopens_under_the_debugger_with_its_declaration() {
    let p = case_project(
        "debug-failed-dap",
        "bad = { args = [\"env\", \"--flavor=x\"], env = { SUITE_MODE = \"loose\" }, cwd = dir(\"tests\") }\nok = { args = [\"ok\"] }\n",
    );
    p.run(".", &["test"]).failure();
    let r = p.run(".", &["test", "--debug-failed", "--dap"]);
    r.success();
    r.stderr_contains("debugging suite:suite/bad");
    // 事例の宣言がそのまま構成になる。手で書き写すものは無い。
    r.stdout_contains("\"--flavor=x\"");
    r.stdout_contains("\"name\": \"SUITE_MODE\"");
    r.stdout_contains("\"value\": \"loose\"");
    r.stdout_contains("tests\"");
    r.stdout_contains("\"miDebuggerPath\": \"gdb\"");
}

#[test]
fn several_failures_ask_to_name_one() {
    // デバッガは対話するものであり、繋がる相手は1つである。こちらが選ぶと、
    // どれが開いたのかを利用者が推測することになる。
    let p = case_project(
        "debug-failed-many",
        "f1 = { args = [\"fail\"] }\nf2 = { args = [\"fail\"] }\n",
    );
    p.run(".", &["test"]).failure();
    let r = p.run(".", &["test", "--debug-failed"]);
    r.failure();
    r.stderr_contains("2 tests failed last time");
    r.stderr_contains("suite:suite/f1");
    r.stderr_contains("suite:suite/f2");
    r.stderr_contains("--debug-failed");
}

#[test]
fn naming_a_case_narrows_the_debug_to_it() {
    // 名指しは通常の選択と同じ形。並べられたラベルを貼り戻せばよい。
    let p = case_project(
        "debug-failed-named",
        "f1 = { args = [\"fail\"] }\nf2 = { args = [\"fail\"] }\n",
    );
    p.run(".", &["test"]).failure();
    let r = p.run(".", &["test", "suite:suite/f2", "--debug-failed", "--dap"]);
    r.success();
    r.stderr_contains("debugging suite:suite/f2");
}

#[test]
fn nothing_failed_means_nothing_to_debug() {
    // 落ちたものが無いのは良い知らせであって、誤りではない。
    let p = case_project("debug-failed-clean", "ok = { args = [\"ok\"] }\n");
    p.run(".", &["test"]).success();
    let r = p.run(".", &["test", "--debug-failed"]);
    r.success();
    r.stderr_contains("nothing to debug");
}

#[test]
fn debug_failed_does_not_combine_with_no_run() {
    // 「走らせない」と「デバッガの下で走らせ直す」は両立しない。
    let p = case_project("debug-failed-norun", "ok = { args = [\"ok\"] }\n");
    let r = p.run(".", &["test", "--debug-failed", "--no-run"]);
    r.failure();
    r.stderr_contains("cannot combine with `--no-run`");
}

/// 事例の選択と可視性（issue #89 / #91 / #93 / #94）。
fn selection_project(name: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"c\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        r#"
[test.suite]
sources = glob("tests/*.c")

[test.suite.cases]
parse   = { args = ["parse"] }
emit    = { args = ["emit"], labels = ["slow"], timeout = 30 }
rejects = { args = ["fail"], should_fail = true }
broken  = { args = ["fail"] }
"#,
    );
    p.write(
        "tests/suite.c",
        "#include <string.h>\nint main(int argc, char **argv) { return argc > 1 && strcmp(argv[1], \"fail\") == 0 ? 3 : 0; }\n",
    );
    p
}

#[test]
fn the_label_a_case_is_reported_under_selects_that_case() {
    // 画面に出た識別子をそのまま貼り戻せる。落ちた1件だけを再実行する経路は
    // ここにしか無い（issue #93）。
    let p = selection_project("select-by-label");
    let r = p.run(".", &["test", "c:suite/broken"]);
    r.failure();
    r.stderr_contains("running 1 test");
    r.stderr_contains("c:suite/broken ... FAILED");
    assert!(!r.stderr.contains("c:suite/parse"), "another case ran\n{r}");
}

#[test]
fn naming_the_target_runs_all_of_its_cases() {
    let p = selection_project("select-by-target");
    let r = p.run(".", &["test", "c:suite"]);
    r.failure();
    r.stderr_contains("running 4 tests");
}

#[test]
fn naming_a_case_that_does_not_exist_does_not_pass_with_zero_tests() {
    let p = selection_project("select-missing-case");
    let r = p.run(".", &["test", "c:suite/nosuch"]);
    r.failure();
    r.stderr_contains("nothing matched");
    r.stderr_contains("--no-run");
}

#[test]
fn naming_a_label_nobody_carries_does_not_pass_with_zero_tests() {
    // stderr の報告は CI のログで埋もれる。状態で伝わらなければ、
    // 「綴りを間違えた」段が緑になる（issue #89）。
    let p = selection_project("select-missing-label");
    let r = p.run(".", &["test", "--label=smok"]);
    r.failure();
    r.stderr_contains("no test carries `smok`");
}

#[test]
fn rerunning_failures_says_so_when_the_remembered_case_is_gone() {
    // 「直す」という行為そのものが、記録と現実を食い違わせる契機になる
    // （issue #91）。
    let p = selection_project("select-failed-gone");
    p.run(".", &["test"]).failure();

    // 落ちた事例を改名する。直したつもりの利用者が `--failed` を打つ。
    p.write(
        "dowel.build",
        "[test.suite]\nsources = glob(\"tests/*.c\")\n\n[test.suite.cases]\nparse = { args = [\"parse\"] }\nfixed = { args = [\"ok\"] }\n",
    );
    let r = p.run(".", &["test", "--failed"]);
    r.failure();
    r.stderr_contains("c:suite/broken");
    r.stderr_contains("no longer exists");
    r.stderr_contains("no remembered failure is still present");
}

#[test]
fn the_cases_that_would_run_can_be_listed_without_running_them() {
    // ラベルの語彙を確かめる先も、重い事例を見分ける先も、ここ以外に無い
    // （issue #94）。
    let p = selection_project("select-list");
    let r = p.run(".", &["test", "--no-run"]);
    r.success();
    // 「組むだけ」の意味は残る。
    r.stderr_contains("built:");
    r.stderr_contains("c:suite/parse");
    r.stderr_contains("c:suite/emit");
    // 事例の属性も見える。何が重く、何が失敗を期待しているか。
    r.stderr_contains("[slow]");
    r.stderr_contains("timeout 30s");
    r.stderr_contains("should_fail");
    // 走ってはいない。
    assert!(!r.stderr.contains("test result:"), "the tests were run\n{r}");
}

#[test]
fn the_listing_honours_the_selection_that_was_asked_for() {
    let p = selection_project("select-list-filtered");
    let r = p.run(".", &["test", "--no-run", "--label=slow"]);
    r.success();
    r.stderr_contains("c:suite/emit");
    assert!(!r.stderr.contains("c:suite/parse"), "the selection was ignored\n{r}");
}

#[test]
fn the_listing_is_machine_readable_with_the_same_labels() {
    // 下流が突き合わせられるよう、走らせたときと同じ欄で出す（issue #100）。
    let p = selection_project("select-list-json");
    let r = p.run(".", &["test", "--no-run", "--message-format=json"]);
    r.success();
    r.stdout_contains("\"kind\":\"test-case\"");
    r.stdout_contains("\"target\":\"c:suite\"");
    r.stdout_contains("\"case\":\"emit\"");
    r.stdout_contains("\"label\":\"c:suite/emit\"");
    r.stdout_contains("\"timeout\":30");
    r.stdout_contains("\"should_fail\":true");
}

/// 事例の宣言の検証（issue #92 / #96 / #97 / #98 / #99 / #101）。
fn cases_decl_project(name: &str, cases_block: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname    = \"c\"\nversion = \"0.1.0\"\n");
    p.write(
        "dowel.build",
        &format!("[test.suite]\nsources = glob(\"tests/*.c\")\n\n{cases_block}"),
    );
    p.write("tests/suite.c", "int main(void) { return 0; }\n");
    p
}

#[test]
fn a_case_can_be_registered_only_for_some_configurations() {
    // 値は分岐できるのに存在は分岐できない、という形だった（issue #92）。
    // 実機でしか意味を持たない事例は、値を変えるのではなく落としたい。
    let p = cases_decl_project(
        "cases-conditional",
        "[test.suite.cases]\nalways  = { args = [\"a\"] }\ndebugly = { args = [\"d\"] } when cfg.opt == \"debug\"\n",
    );
    let r = p.run(".", &["test", "--no-run"]);
    r.success();
    r.stderr_contains("c:suite/always");
    r.stderr_contains("c:suite/debugly");

    let r = p.run(".", &["test", "--no-run", "--config=release"]);
    r.success();
    r.stderr_contains("c:suite/always");
    assert!(!r.stderr.contains("debugly"), "the case was registered anyway\n{r}");
}

#[test]
fn a_case_can_be_chosen_with_match() {
    let p = cases_decl_project(
        "cases-match",
        "[test.suite.cases]\npick = match cfg.opt { debug => { args = [\"d\"], timeout = 30 }, release => { args = [\"r\"] } }\n",
    );
    let r = p.run(".", &["test", "--no-run"]);
    r.success();
    r.stderr_contains("timeout 30s");
    let r = p.run(".", &["test", "--no-run", "--config=release"]);
    r.success();
    assert!(!r.stderr.contains("timeout"), "the release arm carried a timeout\n{r}");
}

#[test]
fn every_arm_of_a_conditional_case_is_still_checked() {
    // 条件は具体化まで解けない。通らない枝の誤りを見逃すと、構成を変えた
    // ときに初めて落ちる。
    let p = cases_decl_project(
        "cases-match-checked",
        "[test.suite.cases]\npick = match cfg.opt { debug => { args = [\"d\"] }, release => { timout = 5 } }\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-property");
    r.stderr_contains("did you mean `timeout`?");
}

#[test]
fn a_case_name_that_breaks_the_label_grammar_is_refused() {
    let p = cases_decl_project("cases-name-slash", "[test.suite.cases]\n\"a/b\" = { args = [] }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("invalid-name");
    r.stderr_contains("separates the target from the case");

    let p = cases_decl_project("cases-name-space", "[test.suite.cases]\n\"x y\" = { args = [] }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("invalid-name");
    r.stderr_contains("whitespace");
}

#[test]
fn a_timeout_that_never_expires_is_refused() {
    // 0 と負は「待ち続ける」に落ちる。時間切れを書いた意図と正反対である。
    let p =
        cases_decl_project("cases-timeout-zero", "[test.suite.cases]\nslow = { timeout = 0 }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("invalid-value");
    r.stderr_contains("positive number of seconds");
}

#[test]
fn an_empty_cases_block_is_not_silently_one_bare_run() {
    // 「事例を書かない」と「事例を書いたが1つも残らなかった」は別の意図である。
    let p = cases_decl_project("cases-empty", "[test.suite.cases]\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("empty-block");
    r.stderr_contains("declares no case");
}

#[test]
fn a_case_written_as_its_own_table_says_what_the_right_shape_is() {
    // 「深すぎる」とだけ言われても、何が正しい形なのかは読み取れない。
    let p = cases_decl_project("cases-own-table", "[test.suite.cases.parse]\nargs = [\"parse\"]\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("too-deep-table");
    r.stderr_contains("inline tables inside it");
}

#[test]
fn a_type_error_underlines_the_key_that_is_wrong() {
    // 事例全体を指すと、どの鍵が悪いのか読み手が探すことになる。
    let p = cases_decl_project(
        "cases-underline",
        "[test.suite.cases]\none = { args = [\"a\"], timeout = \"soon\" }\n",
    );
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("type-mismatch");
    // 下線は `\"soon\"` に付く。事例全体ではない。
    r.stderr_contains("\"soon\"");
}

#[test]
fn a_target_whose_cases_all_dropped_runs_nothing_without_failing() {
    // 条件で空になったのは、書き手の意図と食い違っていない（issue #99 の
    // 「明示的に空」との区別）。
    let p = cases_decl_project(
        "cases-all-dropped",
        "[test.suite.cases]\nonly = { args = [\"d\"] } when cfg.opt == \"debug\"\n",
    );
    let r = p.run(".", &["test", "--config=release"]);
    r.success();
    r.stderr_contains("running 0 tests");
}

#[test]
fn a_shared_library_exports_what_it_declares_and_nothing_else() {
    // 共有ライブラリの書き出す記号は宣言が決める（ADR-0030）。挙げたものが
    // 出て、挙げていないものは——`static` でなく外部結合であっても——出ない。
    // 既定に落とすと ELF では全部出て Windows では何も出ず、同じ宣言が
    // platform ごとに別の interface を意味することになる。
    let p = Project::new("shared-exports");
    p.write("dowel.toml", "[package]\nname = \"shared-exports\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\"]\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [target(\"core\")]\n",
    );
    p.write(
        "src/core.c",
        "int core_open(void) { return 42; }\n\
         int core_internal(void) { return 1; }\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\nint core_open(void);\n\
         int main(void) { printf(\"v=%d\\n\", core_open()); return 0; }\n",
    );

    p.run(".", &["build"]).success();

    let lib_dir = build_dir(&p.path("."), "debug").join("lib");
    let shared = lib_dir.join("libcore.so");
    assert!(shared.is_file(), "no shared library at {}", shared.display());

    // 生成された版指令書がリンクの入力として在ること。
    let script = lib_dir.join("core.map");
    let text = std::fs::read_to_string(&script).expect("no generated version script");
    assert!(text.contains("core_open;") && text.contains("local:"), "{text}");

    // 実際に出ている記号を、出来上がったものに聞く。
    let out = std::process::Command::new("nm")
        .args(["-D", "--defined-only", &shared.display().to_string()])
        .output()
        .expect("nm is not available");
    let symbols = String::from_utf8_lossy(&out.stdout);
    assert!(symbols.contains("core_open"), "core_open is not exported:\n{symbols}");
    assert!(
        !symbols.contains("core_internal"),
        "core_internal was exported although it is not declared:\n{symbols}"
    );

    // 繋いだ実行ファイルが、焼き込んだ探索路で共有ライブラリを見つけて動くこと。
    let bin = build_dir(&p.path("."), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "v=42\n");
}

#[test]
fn a_static_library_inside_a_shared_one_is_compiled_position_independent() {
    // 繋ぎ方の宣言は、依存の翻訳の仕方まで動かす。静的ライブラリの目的コードが
    // 位置独立でなければ、共有ライブラリへの取り込みはリンカに弾かれる
    // （ADR-0030）。
    //
    // 検査はリンクの成否ではなく翻訳の引数で行う。既定で PIE を出す
    // コンパイラでは、多くの目的コードが `-fPIC` 無しでも共有ライブラリに
    // 収まってしまい、症状は組み合わせ次第で消える——決めているのは計画の側
    // なので、そこを見る。
    let p = Project::new("shared-pic-closure");
    p.write("dowel.toml", "[package]\nname = \"shared-pic-closure\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.helper]\nsources = [file(\"src/helper.c\")]\n\n\
         [lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\"]\n\n\
         [lib.core.private]\ndeps = [target(\"helper\")]\n\n\
         [lib.spare]\nsources = [file(\"src/spare.c\")]\n",
    );
    p.write(
        "src/helper.c",
        "int helper_table[4] = {1, 2, 3, 41};\n\
         int *helper_rows(void) { return helper_table; }\n",
    );
    p.write(
        "src/core.c",
        "int *helper_rows(void);\nint core_open(void) { return helper_rows()[3] + 1; }\n",
    );
    p.write("src/spare.c", "int spare_value(void) { return 1; }\n");

    p.run(".", &["build", "core", "spare"]).success();
    assert!(build_dir(&p.path("."), "debug").join("lib/libcore.so").is_file());

    let text =
        std::fs::read_to_string(build_dir(&p.path("."), "debug").join("compile_commands.json"))
            .expect("no compile_commands.json");
    let db: Vec<dowel_support::json::Json> =
        dowel_support::json::parse(&text).unwrap().as_array().unwrap().to_vec();
    let pic_for = |name: &str| -> bool {
        db.iter()
            .find(|e| e.get("file").and_then(|f| f.as_str()).is_some_and(|f| f.ends_with(name)))
            .map(|e| {
                e.get("arguments")
                    .and_then(|a| a.as_array())
                    .unwrap_or(&[])
                    .iter()
                    .any(|a| a.as_str() == Some("-fPIC"))
            })
            .unwrap_or_else(|| panic!("{name} is not in compile_commands.json"))
    };
    assert!(pic_for("core.c"), "the shared library itself is not position independent");
    assert!(
        pic_for("helper.c"),
        "a static library linked into a shared one is not position independent"
    );
    // 共有ライブラリの閉包の外は、従来どおり位置独立にしない。
    assert!(!pic_for("spare.c"), "an unrelated static library was made position independent");
}

#[test]
fn a_shared_library_without_exports_is_refused() {
    // 既定に落とさないことがこの設計の要点である。何を書き出すかは
    // 宣言でなければならない（ADR-0030）。
    let p = Project::new("shared-no-exports");
    p.write("dowel.toml", "[package]\nname = \"shared-no-exports\"\nversion = \"0\"\n");
    p.write("dowel.build", "[lib.core]\nsources = [file(\"src/core.c\")]\nlinkage = \"shared\"\n");
    p.write("src/core.c", "int core_open(void) { return 1; }\n");

    p.run(".", &["build"]).failure().stderr_contains("missing-exports");
}

/// 依存パッケージが自分の検査を持つ木（issue #126）。
///
/// 本体のフィクスチャにこの形が無かった。依存を持つものは「使う側が
/// 依存の成果物を引けること」を見るので依存側に `test` を置く理由が無く、
/// ライブラリの検査を見るものは単独のパッケージになる。
fn a_dependency_that_has_its_own_tests(name: &str) -> Project {
    let p = Project::new(name);
    p.write("mylib/dowel.toml", "[package]\nname = \"mylib\"\nversion = \"0\"\n");
    p.write(
        "mylib/dowel.build",
        "[lib.mylib]\nsources = [file(\"src/mylib.c\")]\n\n\
         [lib.mylib.public]\nincludes = [dir(\"include\")]\n\n\
         [test.libcheck]\nsources = [file(\"tests/libcheck.c\")]\n\n\
         [test.libcheck.private]\ndeps = [target(\"mylib\")]\n",
    );
    p.write("mylib/include/mylib.h", "#pragma once\nint mylib_add(int, int);\n");
    p.write(
        "mylib/src/mylib.c",
        "#include \"mylib.h\"\nint mylib_add(int a, int b) { return a + b; }\n",
    );
    p.write(
        "mylib/tests/libcheck.c",
        "#include \"mylib.h\"\n#include <stdio.h>\n\
         int main(void) { printf(\"ok\\n\"); return mylib_add(1, 1) == 2 ? 0 : 1; }\n",
    );
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"mylib\"\npath = \"../mylib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"mylib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include \"mylib.h\"\n#include <stdio.h>\n\
         int main(void) { printf(\"v=%d\\n\", mylib_add(20, 22)); return 0; }\n",
    );
    p
}

#[test]
fn building_a_consumer_does_not_build_the_dependencys_own_tests() {
    // 使う側の build は、依存パッケージの検査まで組んでいた。ホストの
    // 載った三つ組では余計なだけだが、OS の無い三つ組では**落ちる**
    // ——依存の検査はホスト用に書かれており、使う側のマニフェストには
    // 何の誤りも無い（issue #126）。
    let p = a_dependency_that_has_its_own_tests("consumer-skips-dep-tests");
    p.run("app", &["build"]).success();

    let bin = build_dir(&p.path("app"), "debug").join("bin");
    assert!(bin.join("app").is_file(), "the consumer's own binary was not built");
    assert!(
        !bin.join("libcheck").exists(),
        "the dependency's test was built by the consumer's build"
    );
    // 依存のライブラリ自体は当然要る。
    assert_eq!(run_artifact(&bin.join("app")), "v=42\n");
}

#[test]
fn testing_a_consumer_does_not_run_the_dependencys_own_tests() {
    // `test` も同じ立場である。依存の検査は依存の作者が走らせるもので、
    // 使う側の `dowel test` が走らせる理由は薄い。
    let p = a_dependency_that_has_its_own_tests("consumer-skips-dep-test-run");
    let r = p.run("app", &["test"]);
    r.success();
    assert!(!r.stderr.contains("libcheck"), "the dependency's test was run:\n{}", r.stderr);
}

#[test]
fn a_dependencys_tests_still_run_in_its_own_package() {
    // 既定を絞っても、ライブラリの作者が自分の検査を走らせる道は変わらない。
    let p = a_dependency_that_has_its_own_tests("dep-tests-in-place");
    let r = p.run("mylib", &["test"]);
    r.success();
    assert!(r.stderr.contains("libcheck"), "the library's own test did not run:\n{}", r.stderr);
}

#[test]
fn a_target_can_name_the_triples_it_is_built_for() {
    // 複数の三つ組を支えるライブラリが、自分の検査だけをホストの載った
    // 三つ組に絞れること（issue #126）。`[package] targets` はパッケージ
    // 全体に掛かるので、ここには使えない——支えるのは全部、検査が動くのは
    // 一部、という形が書けなかった。
    let p = Project::new("per-target-triples");
    // クロスの三つ組には道具立ての宣言が要る。ここで見たいのは目標の
    // 絞り込みだけなので、ホストの道具をそのまま名指しする。
    p.write(
        "dowel.toml",
        "[package]\nname = \"per-target-triples\"\nversion = \"0\"\n\n\
         [toolchain.thumbv7em-none-eabihf]\nc = \"cc\"\nar = \"ar\"\n",
    );
    p.write(
        "dowel.build",
        &format!(
            "[lib.core]\nsources = [file(\"src/core.c\")]\n\n\
             [test.vectors]\n\
             sources = [file(\"tests/vectors.c\")]\n\
             targets = [\"{}\"]\n\n\
             [test.vectors.private]\ndeps = [target(\"core\")]\n",
            host_triple()
        ),
    );
    p.write("src/core.c", "int core_add(int a, int b) { return a + b; }\n");
    p.write(
        "tests/vectors.c",
        "#include <stdio.h>\nint core_add(int, int);\n\
         int main(void) { printf(\"ok\\n\"); return core_add(1, 1) == 2 ? 0 : 1; }\n",
    );

    // ホストの三つ組では、従来どおり数え上げられて走る。
    let r = p.run(".", &["test"]);
    r.success();
    assert!(r.stderr.contains("vectors"), "the test did not run on its own triple:\n{}", r.stderr);

    // 挙げられていない三つ組では、名指ししなければ**現れない**。
    // `unsupported-target` で落ちるのではなく、対象外として外れる。
    let r = p.run(".", &["build", "--target=thumbv7em-none-eabihf"]);
    r.success();
    assert!(
        !r.stderr.contains("vectors"),
        "the test was built for a triple it does not name:\n{}",
        r.stderr
    );
}

#[test]
fn naming_a_target_outside_its_triples_is_refused() {
    // 既定から外すのと、名指しを黙って無視するのは別である。名指しは
    // 要求であり、応えられないなら断る——黙って何も作らずに成功すると、
    // 「組んだつもり」が残る。
    let p = Project::new("per-target-triples-named");
    p.write(
        "dowel.toml",
        "[package]\nname = \"per-target-triples-named\"\nversion = \"0\"\n\n\
         [toolchain.thumbv7em-none-eabihf]\nc = \"cc\"\nar = \"ar\"\n",
    );
    p.write(
        "dowel.build",
        &format!(
            "[bin.hosted]\nsources = [file(\"src/main.c\")]\ntargets = [\"{}\"]\n",
            host_triple()
        ),
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["build", "hosted", "--target=thumbv7em-none-eabihf"]);
    r.failure();
    r.stderr_contains("unsupported-target");
}

#[test]
fn the_missing_toolchain_error_reads_out_what_a_dependency_declares() {
    // 「無い」と言いながら、同じ出力の中で `toolchain-mismatch` が値を
    // 読み上げていた。助言は一般論を出し、具体的な答は手元にある
    // ——立場を説明していない出力だった（issue #125）。
    let p = Project::new("dep-toolchain-note");
    p.write(
        "mylib/dowel.toml",
        "[package]\nname = \"mylib\"\nversion = \"0\"\n\n\
         [toolchain.aarch64-unknown-linux-gnu]\n\
         c  = \"aarch64-linux-gnu-gcc\"\n\
         ar = \"aarch64-linux-gnu-ar\"\n",
    );
    p.write("mylib/dowel.build", "[lib.mylib]\nsources = [file(\"src/mylib.c\")]\n");
    p.write("mylib/src/mylib.c", "int mylib_add(int a, int b) { return a + b; }\n");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"mylib\"\npath = \"../mylib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"mylib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "int mylib_add(int, int);\nint main(void) { return mylib_add(1, 1) == 2 ? 0 : 1; }\n",
    );

    let r = p.run("app", &["build", "--target=aarch64-unknown-linux-gnu"]);
    r.failure();
    r.stderr_contains("missing-toolchain");
    // 具体的な値を述べること。
    r.stderr_contains("dependency `mylib` declares one for this triple");
    r.stderr_contains("aarch64-linux-gnu-gcc");
    // なぜ効かないのかを述べること。探す時間が要るのはこの一文が無いためである。
    r.stderr_contains("a property of the build, not of a package");
    // 手元に答があるときは、一般論の助言を出さない。
    assert!(
        !r.stderr.contains("for example `[toolchain."),
        "the generic advice was printed although the specific answer was at hand:\n{}",
        r.stderr
    );
}

#[test]
fn a_composed_predicate_selects_the_same_value_under_several_conditions() {
    // 「Linux または macOS」を二行に分けて書いていた。二行は1つの意図で
    // あり、片方だけ直しても何も言わない——読み手は右辺が一致することに
    // 気付いて初めて論理和と読める（ADR-0032）。
    let p = Project::new("composed-predicate");
    p.write("dowel.toml", "[package]\nname = \"composed-predicate\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\n\
         defines = { ON_UNIX = 1 } when target.os == \"linux\" or target.os == \"macos\"\n\
         flags   = [\"-DNOT_WINDOWS\"] when not target.os == \"windows\"\n\
         # 括弧が無ければ `and` が先に畳まれ、debug 以外でも付いてしまう\n\
         c_flags = [\"-DGROUPED\"] \
         when (target.os == \"linux\" or target.os == \"macos\") and cfg.opt == \"debug\"\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\n\
         int main(void) {\n\
         #ifdef ON_UNIX\n  printf(\"unix\\n\");\n#endif\n\
         #ifdef NOT_WINDOWS\n  printf(\"notwin\\n\");\n#endif\n\
         #ifdef GROUPED\n  printf(\"grouped\\n\");\n#endif\n\
           return 0;\n}\n",
    );

    p.run(".", &["build"]).success();
    let bin = build_dir(&p.path("."), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "unix\nnotwin\ngrouped\n");

    // release では、括弧の中は真のまま `and` の右が偽になる。
    p.run(".", &["build", "--config=release"]).success();
    let bin = build_dir(&p.path("."), "release").join("bin/app");
    assert_eq!(run_artifact(&bin), "unix\nnotwin\n");
}

#[test]
fn a_misspelling_inside_a_composed_predicate_is_reported_at_the_leaf() {
    // 合成の中で綴りを誤った鍵は、簡単な述語で誤ったのと同じだけ誤って
    // いる。葉で言う方が、指す先が誤字そのものになる（ADR-0032）。
    let p = Project::new("composed-predicate-typo");
    p.write("dowel.toml", "[package]\nname = \"composed-predicate-typo\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\n\
         flags = [\"-DX\"] when target.os == \"windwos\" or cfg.opt == \"debug\"\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-pattern");
    r.stderr_contains("windwos");
    r.stderr_contains("did you mean `windows`");
}

/// 複数の使う側が1つの道具立ての表を共有する木（issue #125、ADR-0033）。
fn a_tree_sharing_one_toolchain_file(name: &str) -> Project {
    let p = Project::new(name);
    p.write(
        "toolchains.toml",
        "[toolchain.thumbv7em-none-eabihf]\nc  = \"cc\"\nar = \"ar\"\n\n\
         [toolchain.aarch64-unknown-linux-gnu]\n\
         c  = \"aarch64-linux-gnu-gcc\"\nar = \"aarch64-linux-gnu-ar\"\n",
    );
    for pkg in ["cli", "fw"] {
        p.write(
            &format!("{pkg}/dowel.toml"),
            &format!(
                "[package]\nname = \"{pkg}\"\nversion = \"0\"\n\
                 toolchains = \"../toolchains.toml\"\n"
            ),
        );
        p.write(
            &format!("{pkg}/dowel.build"),
            &format!("[bin.{pkg}]\nsources = [file(\"src/main.c\")]\n"),
        );
        p.write(&format!("{pkg}/src/main.c"), "int main(void) { return 0; }\n");
    }
    p
}

#[test]
fn a_shared_toolchain_file_supplies_the_declaration() {
    // 使う側それぞれが同じ表を写していた。1箇所に置いて名指しできる
    // （ADR-0033）。
    let p = a_tree_sharing_one_toolchain_file("shared-toolchain");
    for pkg in ["cli", "fw"] {
        p.run(pkg, &["build", "--target=thumbv7em-none-eabihf"]).success();
        assert!(build_dir(&p.path(pkg), "debug")
            .parent()
            .unwrap()
            .join("thumbv7em-none-eabihf-debug/bin")
            .join(pkg)
            .is_file());
    }
}

#[test]
fn a_local_declaration_overrides_one_tool_of_the_shared_file() {
    // 上書きの単位は道具1つである。三つ組ごとにすると、1つの道具を替える
    // ために表全体を写し直すことになり、この機構の目的に反する（ADR-0033）。
    let p = a_tree_sharing_one_toolchain_file("shared-toolchain-override");
    p.write(
        "cli/dowel.toml",
        "[package]\nname = \"cli\"\nversion = \"0\"\n\
         toolchains = \"../toolchains.toml\"\n\n\
         [toolchain.thumbv7em-none-eabihf]\nc = \"nonexistent-cc\"\n",
    );
    let r = p.run("cli", &["build", "--target=thumbv7em-none-eabihf"]);
    r.failure();
    // ローカルの `c` が勝ち、位置もそこを指す。
    r.stderr_contains("nonexistent-cc");
    r.stderr_contains("cli/dowel.toml");
    // `ar` は共有ファイルのまま。上書きされた道具に巻き込まれていない。
    assert!(!r.stderr.contains("cannot find the archiver"), "{}", r.stderr);
}

#[test]
fn a_missing_toolchain_file_is_reported_where_it_is_named() {
    let p = a_tree_sharing_one_toolchain_file("shared-toolchain-missing");
    p.write(
        "cli/dowel.toml",
        "[package]\nname = \"cli\"\nversion = \"0\"\ntoolchains = \"../nowhere.toml\"\n",
    );
    let r = p.run("cli", &["check"]);
    r.failure();
    r.stderr_contains("unreadable-toolchains");
    r.stderr_contains("relative to the `dowel.toml` that names it");
}

#[test]
fn a_toolchain_file_holds_toolchains_only() {
    // 他の表を黙って無視すると、「dowel.toml のつもりで書いた」ことと
    // 「何も起きなかった」ことが同じに見える（ADR-0033）。
    let p = a_tree_sharing_one_toolchain_file("shared-toolchain-stray");
    p.write(
        "toolchains.toml",
        "[package]\nname = \"oops\"\n\n[toolchain.thumbv7em-none-eabihf]\nc = \"cc\"\n",
    );
    let r = p.run("cli", &["check", "--target=thumbv7em-none-eabihf"]);
    r.failure();
    r.stderr_contains("is not read from a toolchain file");
}

#[test]
fn a_dependencys_shared_toolchain_file_is_not_read_either() {
    // ADR-0031 の立場は変わらない。この ADR が与えるのは、使う側が表を
    // 1度だけ書く場所であって、継ぐ手段ではない。
    let p = Project::new("shared-toolchain-not-inherited");
    p.write("toolchains.toml", "[toolchain.thumbv7em-none-eabihf]\nc = \"cc\"\nar = \"ar\"\n");
    p.write(
        "mylib/dowel.toml",
        "[package]\nname = \"mylib\"\nversion = \"0\"\ntoolchains = \"../toolchains.toml\"\n",
    );
    p.write("mylib/dowel.build", "[lib.mylib]\nsources = [file(\"src/mylib.c\")]\n");
    p.write("mylib/src/mylib.c", "int mylib_add(int a, int b) { return a + b; }\n");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"mylib\"\npath = \"../mylib\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"mylib\")]\n",
    );
    p.write(
        "app/src/main.c",
        "int mylib_add(int, int);\nint main(void) { return mylib_add(1, 1) == 2 ? 0 : 1; }\n",
    );

    let r = p.run("app", &["build", "--target=thumbv7em-none-eabihf"]);
    r.failure();
    r.stderr_contains("missing-toolchain");
}

#[test]
fn an_unknown_configuration_key_says_where_a_projects_own_axis_goes() {
    // 「無い」とだけ言う診断は、何を書けばよいかを述べない。dowel が知らない
    // 軸で分岐したい人は正当な要求を持っており、置き場所は前から在る
    // （ADR-0034）。以前は「語彙は暫定、Q1 を見よ」と答えていた。
    let p = Project::new("closed-vocabulary");
    p.write("dowel.toml", "[package]\nname = \"closed-vocabulary\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\n\
         flags = [\"-fsanitize=address\"] when cfg.sanitizer == \"address\"\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-cfg-key");
    r.stderr_contains("the vocabulary is closed");
    // 打ち間違いとして近い鍵が無いので、名前を当てはめて導く。
    r.stderr_contains("declare `sanitizer` in `[features]`");
    r.stderr_contains("write `feature.sanitizer`");
    // 開いていた頃の案内は残っていない。
    assert!(!r.stderr.contains("provisional"), "{}", r.stderr);
}

#[test]
fn following_the_suggestion_lands_on_the_next_correct_step() {
    // 2段の導線であること。`unknown-cfg-key` は `dowel.build` の評価中に
    // 出るので `[features]` を読めない——宣言の有無を知る段が次に答える。
    let p = Project::new("closed-vocabulary-next-step");
    p.write("dowel.toml", "[package]\nname = \"closed-vocabulary-next-step\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nflags = [\"-fsanitize=address\"] when feature.sanitizer\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-feature");
    r.stderr_contains("not declared in `dowel.toml`");

    // 宣言すれば通る。導線の行き先が実際に有効であること。
    p.write(
        "dowel.toml",
        "[package]\nname = \"closed-vocabulary-next-step\"\nversion = \"0\"\n\n\
         [features]\nsanitizer = []\n",
    );
    p.run(".", &["check"]).success();
}

#[test]
fn a_misspelled_key_still_gets_the_near_one() {
    // 導線を足しても、打ち間違いの提案が押しのけられないこと。
    let p = Project::new("closed-vocabulary-typo");
    p.write("dowel.toml", "[package]\nname = \"closed-vocabulary-typo\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nflags = [\"-DX\"] when cfg.taget == \"x\"\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("did you mean `target`?");
    assert!(!r.stderr.contains("feature.taget"), "{}", r.stderr);
}

#[test]
fn a_template_shares_a_private_setting_without_publishing_it() {
    // これがテンプレートの存在理由である（ADR-0035）。ソースの無い lib に
    // 依存する書き方でも設定は配れるが、配れるのは `public` だけで、
    // それは「共有する」ではなく「公開する」——依存側の全員に届く。
    let p = Project::new("template-private");
    p.write("dowel.toml", "[package]\nname = \"template-private\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[template.tool]\n\n\
         [template.tool.private]\ndefines = { PRIVATE_ONLY = 1 }\n\n\
         [lib.core]\nsources = [file(\"src/core.c\")]\nuse = [template(\"tool\")]\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [target(\"core\")]\n",
    );
    // テンプレートを使った側には届く。
    p.write(
        "src/core.c",
        "#ifndef PRIVATE_ONLY\n#error the template's private did not reach the target\n#endif\n\
         int core_value(void) { return 42; }\n",
    );
    // その依存側には届かない。`public` に置いたのでは、こうならない。
    p.write(
        "src/main.c",
        "#ifdef PRIVATE_ONLY\n#error the private setting leaked to the dependent\n#endif\n\
         #include <stdio.h>\nint core_value(void);\n\
         int main(void) { printf(\"v=%d\\n\", core_value()); return 0; }\n",
    );

    p.run(".", &["build"]).success();
    let bin = build_dir(&p.path("."), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "v=42\n");
}

#[test]
fn a_template_expands_ahead_of_the_targets_own_lines() {
    // 展開は「テンプレートの行が先に書かれていた」のと同じである。
    // 併合の代数に特例を作らないので、`append` の順序も普段どおり。
    let p = Project::new("template-order");
    p.write("dowel.toml", "[package]\nname = \"template-order\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[template.warn]\n\n\
         [template.warn.private]\nflags = [\"-DFROM_TEMPLATE\"]\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\nuse = [template(\"warn\")]\n\n\
         [bin.app.private]\nflags = [\"-DFROM_TARGET\"]\n",
    );
    p.write(
        "src/main.c",
        "#if !defined(FROM_TEMPLATE) || !defined(FROM_TARGET)\n#error both should reach\n#endif\n\
         int main(void) { return 0; }\n",
    );

    p.run(".", &["build"]).success();
    let text =
        std::fs::read_to_string(build_dir(&p.path("."), "debug").join("compile_commands.json"))
            .expect("no compile_commands.json");
    let t = text.find("-DFROM_TEMPLATE").expect("the template flag is missing");
    let o = text.find("-DFROM_TARGET").expect("the target flag is missing");
    assert!(t < o, "the template's flag should come first");
}

#[test]
fn a_template_holds_settings_only() {
    // root のプロパティは「そのターゲットが何であるか」を決める。共有すると
    // 何を作っているのかが読み取れなくなる。`use` を書けないことが、
    // テンプレートが再帰しないことでもある。
    let p = Project::new("template-settings-only");
    p.write("dowel.toml", "[package]\nname = \"template-settings-only\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[template.tool]\nsources = [file(\"src/main.c\")]\nuse = [template(\"other\")]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("a template has no `sources`");
    r.stderr_contains("a template has no `use`");
    r.stderr_contains("templates hold settings only");
}

#[test]
fn a_template_is_not_something_to_build() {
    let p = Project::new("template-not-a-target");
    p.write("dowel.toml", "[package]\nname = \"template-not-a-target\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[template.tool]\n\n[template.tool.private]\nflags = [\"-DX\"]\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\nuse = [template(\"tool\")]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    // 名指しは断る。
    let r = p.run(".", &["build", "tool"]);
    r.failure();
    r.stderr_contains("not-a-target");
    // 名指ししなければ、テンプレートは数えられず app だけが組まれる。
    p.run(".", &["build"]).success();
    assert!(build_dir(&p.path("."), "debug").join("bin/app").is_file());
}

#[test]
fn an_unknown_template_names_the_declared_ones() {
    let p = Project::new("template-unknown");
    p.write("dowel.toml", "[package]\nname = \"template-unknown\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[template.tool]\n\n[template.tool.private]\nflags = [\"-DX\"]\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\nuse = [template(\"tol\")]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-template");
    r.stderr_contains("did you mean `tool`?");
}

#[test]
fn a_draft_imported_from_meson_builds_without_editing() {
    // Meson の `parameters` にはリンクと書庫の引数が混ざる。仕分けずに
    // `flags` へ入れると `cc` が入力ファイルとして読み、下書きはそのままでは
    // **組めない**（issue #135）。読めることだけを見ていては捕まらない。
    let p = Project::new("meson-import-buildable");
    let src = p.path(".").display().to_string();
    p.write("include/shapes.h", "#pragma once\nint area(int a);\n");
    p.write("src/area.c", "#include \"shapes.h\"\nint area(int a) { return a * a; }\n");
    p.write(
        "src/main.c",
        "#include <stdio.h>\n#include \"shapes.h\"\n\
         int main(void) { printf(\"a=%d\\n\", area(3)); return 0; }\n",
    );

    // `static_library` + `executable` + `link_with` の普通の木。ここで初めて
    // `ar` の引数とリンカの引数が配列に現れる。
    p.write(
        "build/meson-info/intro-projectinfo.json",
        r#"{"version": "0.1", "descriptive_name": "shapes", "subprojects": []}"#,
    );
    p.write(
        "build/meson-info/intro-targets.json",
        &format!(
            r#"[
              {{"name": "shapes", "type": "static library", "defined_in": "{src}/meson.build",
                "subproject": null,
                "target_sources": [{{"language": "c", "compiler": ["cc"],
                  "parameters": ["-I{src}/include", "-I", "-DSHAPES_BUILD=1", "-Wall",
                                 "-fdiagnostics-color=always", "csrDT"],
                  "sources": ["{src}/src/area.c"], "generated_sources": []}}]}},
              {{"name": "shapetool", "type": "executable", "defined_in": "{src}/meson.build",
                "subproject": null,
                "target_sources": [{{"language": "c", "compiler": ["cc"],
                  "parameters": ["-I{src}/include", "-DTOOL=1", "-Wall",
                                 "-Wl,--as-needed", "-Wl,--start-group", "libshapes.a",
                                 "-Wl,--end-group"],
                  "sources": ["{src}/src/main.c"], "generated_sources": []}}]}}
            ]"#
        ),
    );

    p.run(".", &["migrate", "import", "build"]).success();
    let build_file = std::fs::read_to_string(p.path("dowel.build")).unwrap();

    // 翻訳の引数だけが `flags` に残る。
    for line in build_file.lines().filter(|l| l.trim_start().starts_with("flags")) {
        for stray in ["csrDT", "libshapes.a", "-Wl,"] {
            assert!(!line.contains(stray), "`{stray}` is not a compile flag:\n{line}");
        }
    }
    // リンカの引数は link_flags へ。
    assert!(build_file.contains("-Wl,--as-needed"), "{build_file}");
    // 落とした入力は名前が残る。`deps` に書き直すのは読み手の仕事である。
    assert!(build_file.contains("libshapes.a"), "the dropped input should be named");
    assert!(build_file.contains("belong in `deps`"), "the header should say why");
    // 空の `-I` は `dir("")` にしない。
    assert!(!build_file.contains("dir(\"\")"), "{build_file}");

    // そして実際に組める。`shapes` は自足しており、`shapetool` は依存を
    // 書き足すまで繋がらないので、ここではライブラリを組んで確かめる。
    p.run(".", &["build", "shapes"]).success();
}

#[test]
fn a_shared_librarys_own_tests_reach_inside_it() {
    // 内側を見る検査は、ライブラリの検査として普通の形である。公開の面
    // だけを叩く検査は、面の後ろの表や状態機械を覆えない（issue #134）。
    //
    // `exports` は「一緒に書かれていないコード」に対する境界であり、
    // 兄弟のターゲットは配る相手ではない——パッケージが配布の単位だから
    // である（ADR-0038）。
    let p = Project::new("shared-own-tests");
    p.write("dowel.toml", "[package]\nname = \"shared-own-tests\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\"]\n\n\
         [test.unit]\nsources = [file(\"tests/unit.c\")]\n\n\
         [test.unit.private]\ndeps = [target(\"core\")]\n",
    );
    p.write(
        "src/core.c",
        "int core_step(int x) { return x + 1; }\n\
         int core_open(void) { return core_step(41); }\n",
    );
    // 公開の面と、面に無い内部の名前の両方を呼ぶ。
    p.write(
        "tests/unit.c",
        "int core_open(void);\nint core_step(int);\n\
         int main(void) { return (core_open() == 42 && core_step(1) == 2) ? 0 : 1; }\n",
    );

    let r = p.run(".", &["test"]);
    r.success();
    assert!(r.stderr.contains("unit"), "the library's own test should run:\n{}", r.stderr);

    // 面は変わっていない。配る相手から見えるのは `exports` だけである。
    p.run(".", &["build"]).success();
    let lib = build_dir(&p.path("."), "debug").join("lib/libcore.so");
    assert!(lib.is_file(), "the shared library is still built");
    let out = std::process::Command::new("nm")
        .args(["-D", "--defined-only", &lib.display().to_string()])
        .output()
        .expect("nm is not available");
    let symbols = String::from_utf8_lossy(&out.stdout);
    assert!(symbols.contains("core_open"), "{symbols}");
    assert!(
        !symbols.contains("core_step"),
        "the internal name must stay off the surface:\n{symbols}"
    );
}

#[test]
fn a_consumer_in_another_package_still_sees_only_the_surface() {
    // 対になる検査。境界は残っている——別のパッケージからは面越しである。
    let p = Project::new("shared-across-packages");
    p.write("core/dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "core/dowel.build",
        "[lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\"]\n\n\
         [lib.core.public]\nincludes = [dir(\"include\")]\n",
    );
    p.write("core/include/core.h", "#pragma once\nint core_open(void);\nint core_step(int);\n");
    p.write(
        "core/src/core.c",
        "int core_step(int x) { return x + 1; }\n\
         int core_open(void) { return core_step(41); }\n",
    );
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"core\"\npath = \"../core\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\n",
    );
    // 面にあるものだけを呼ぶ使う側は組めて、走る。
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"v=%d\\n\", core_open()); return 0; }\n",
    );
    p.run("app", &["build"]).success();
    assert_eq!(run_artifact(&build_dir(&p.path("app"), "debug").join("bin/app")), "v=42\n");

    // 面に無いものを呼ぶと、別のパッケージからは繋がらない。
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"v=%d\\n\", core_step(1)); return 0; }\n",
    );
    p.run("app", &["build"]).failure();
}

#[test]
fn a_misspelled_export_is_caught_where_it_is_declared() {
    // `exports` の誤字はビルドを通り、誤った名前はただ動的記号表に現れない。
    // 失敗は**他人のビルド**で、ヘッダに見えている関数への undefined
    // reference として出る（ADR-0039）。
    //
    // リンカには頼めない——共有ライブラリは未定義記号を持ちうるので、
    // `-Wl,-u` も `--no-undefined` も欠けた記号を誤りにしない。出来上がった
    // ものに聞くしかない。
    let p = Project::new("exports-typo");
    p.write("dowel.toml", "[package]\nname = \"exports-typo\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\", \"core_opne\"]\n",
    );
    p.write("src/core.c", "int core_open(void) { return 42; }\n");

    let r = p.run(".", &["build", "core"]);
    r.failure();
    r.stderr_contains("unexported-symbol");
    r.stderr_contains("core_opne");
    // 近い名前が在れば挙げる。誤字の直し先はたいていそこにある。
    r.stderr_contains("does export `core_open`");
    // なぜ黙っていたかを述べる。
    r.stderr_contains("until a consumer fails to link");
}

#[test]
fn a_correct_export_list_says_nothing() {
    // 対になる検査。これが無いと、上の検査は「常に落ちる」でも通る。
    let p = Project::new("exports-correct");
    p.write("dowel.toml", "[package]\nname = \"exports-correct\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\", \"core_close\"]\n",
    );
    p.write(
        "src/core.c",
        "int core_internal(void) { return 1; }\n\
         int core_open(void) { return core_internal() + 41; }\n\
         int core_close(void) { return 0; }\n",
    );

    let r = p.run(".", &["build", "core"]);
    r.success();
    assert!(!r.stderr.contains("unexported-symbol"), "stderr:\n{}", r.stderr);
}

#[test]
fn a_declared_soversion_names_the_library_and_what_consumers_record() {
    // 版を書くと、実体の名前と soname の両方がそれになる（ADR-0040）。
    // 使う側が記録するのは soname であり、版を持たない名前を記録した
    // 実行ファイルは、次の世代が同じ名前で置かれた時点で黙って壊れる。
    let p = Project::new("soversion");
    p.write("core/dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "core/dowel.build",
        "[lib.core]\n\
         sources   = [file(\"src/core.c\")]\n\
         linkage   = \"shared\"\n\
         soversion = 2\n\
         exports   = [\"core_open\"]\n\n\
         [lib.core.public]\nincludes = [dir(\"include\")]\n",
    );
    p.write("core/include/core.h", "#pragma once\nint core_open(void);\n");
    p.write("core/src/core.c", "int core_open(void) { return 42; }\n");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"core\"\npath = \"../core\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"v=%d\\n\", core_open()); return 0; }\n",
    );

    p.run("app", &["build"]).success();
    let out = build_dir(&p.path("app"), "debug");
    let versioned = out.join("lib/libcore.so.2");
    assert!(versioned.is_file(), "no versioned library at {}", versioned.display());
    assert!(!out.join("lib/libcore.so").is_file() || out.join("lib/libcore.so").is_symlink());

    // 版を持たない名前は実体の隣に置かれ、実体を指す。`-lcore` が
    // 見つけるのはこの名前である。
    let alias = out.join("lib/libcore.so");
    let points_at = std::fs::read_link(&alias).expect("no unversioned name beside the library");
    assert_eq!(points_at, std::path::Path::new("libcore.so.2"), "{points_at:?}");

    // 使う側が記録した名前を、出来上がったものから読む。
    let bin = std::fs::read(out.join("bin/app")).expect("cannot read the executable");
    assert!(
        bin.windows(13).any(|w| w == b"libcore.so.2\0"),
        "the executable does not record the versioned name"
    );
    assert_eq!(run_artifact(&out.join("bin/app")), "v=42\n");

    // 版を持たない名前を消しても走る。記録されているのは実体の名前で
    // あって、別名ではない。
    std::fs::remove_file(&alias).expect("cannot remove the unversioned name");
    assert_eq!(run_artifact(&out.join("bin/app")), "v=42\n");
}

#[test]
fn without_a_soversion_the_library_keeps_its_plain_name() {
    // 対になる検査。版は書いた者にだけ付く——既定で番号を振ると、dowel が
    // 何も確かめていない数を名前に押し込むことになる（ADR-0040）。
    let p = Project::new("soversion-absent");
    p.write("dowel.toml", "[package]\nname = \"plain\"\nversion = \"1.2.3\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\"]\n",
    );
    p.write("src/core.c", "int core_open(void) { return 42; }\n");

    p.run(".", &["build", "core"]).success();
    let lib_dir = build_dir(&p.path("."), "debug").join("lib");
    assert!(lib_dir.join("libcore.so").is_file(), "the plain name is the artifact");
    assert!(!lib_dir.join("libcore.so").is_symlink(), "and it is not a link to something else");
    // パッケージの版は名前に入らない。配布の版と ABI の世代は別物である。
    assert!(!lib_dir.join("libcore.so.1").exists(), "the package version is not the ABI's");
}

#[test]
fn a_negative_soversion_is_refused_where_it_is_written() {
    let p = Project::new("soversion-negative");
    p.write("dowel.toml", "[package]\nname = \"neg\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\n\
         sources   = [file(\"src/core.c\")]\n\
         linkage   = \"shared\"\n\
         soversion = -1\n\
         exports   = [\"core_open\"]\n",
    );
    p.write("src/core.c", "int core_open(void) { return 42; }\n");

    let r = p.run(".", &["build", "core"]);
    r.failure();
    r.stderr_contains("invalid-soversion");
    r.stderr_contains("dowel.build");
}

#[test]
fn install_copies_the_products_and_they_run_without_the_build_tree() {
    // 共有ライブラリを宣言する目的は配ることであり、配る手段が無ければ
    // 宣言は途中で終わっている（ADR-0041）。
    //
    // 肝は実行時の探索路である。ビルドディレクトリの絶対パスだけを記録
    // した実行ファイルは、ビルド木が在る限り動いてしまうので、壊れて
    // いることが配った先で分かる。
    let p = Project::new("install");
    p.write("core/dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "core/dowel.build",
        "[lib.core]\n\
         sources   = [file(\"src/core.c\")]\n\
         linkage   = \"shared\"\n\
         soversion = 3\n\
         exports   = [\"core_open\"]\n\n\
         [lib.core.public]\nincludes = [dir(\"include\")]\n",
    );
    p.write("core/include/core.h", "#pragma once\nint core_open(void);\n");
    p.write("core/include/core/detail.h", "#pragma once\n");
    p.write("core/src/core.c", "int core_open(void) { return 42; }\n");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"core\"\npath = \"../core\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\n\n\
         [test.smoke]\nsources = [file(\"src/main.c\")]\n\n\
         [test.smoke.private]\ndeps = [dep(\"core\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"v=%d\\n\", core_open()); return 0; }\n",
    );

    let prefix = p.path("out");
    let r = p.run("app", &["install", &format!("--prefix={}", prefix.display())]);
    r.success();

    assert!(prefix.join("bin/app").is_file(), "the executable is installed");
    assert!(prefix.join("lib/libcore.so.3").is_file(), "the library it needs comes along");
    // 版を持たない名前も添える。`-lcore` が見つけるのはこの名前である。
    assert!(prefix.join("lib/libcore.so").is_symlink(), "the unversioned name is placed too");
    // 検査は物を確かめる道具であって、配る物ではない。
    assert!(!prefix.join("bin/smoke").exists(), "a test is not installed");

    // ビルド木を消してから走らせる。ここが本題である。
    std::fs::remove_dir_all(p.path("app/.dowel")).expect("cannot remove the build tree");
    assert_eq!(run_artifact(&prefix.join("bin/app")), "v=42\n");
}

#[test]
fn installing_a_library_brings_the_headers_it_publishes() {
    // `public.includes` は「使う側の探索路に載る」と述べた宣言である。
    // そこから辿れるものは既に面であり、写すのは推測ではない（ADR-0041）。
    let p = Project::new("install-headers");
    p.write("dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\nsources = [file(\"src/core.c\")]\n\n\
         [lib.core.public]\nincludes = [dir(\"include\")]\n",
    );
    p.write("include/core.h", "#pragma once\nint core_open(void);\n");
    p.write("include/core/detail.h", "#pragma once\n");
    p.write("src/core.c", "int core_open(void) { return 42; }\n");

    // `--destdir` は先頭に付くだけである。段取り用のディレクトリへ入れて
    // から `prefix` へ移しても、同じものが動く。
    let staged = p.path("staged");
    let r =
        p.run(".", &["install", "--prefix=/usr/local", &format!("--destdir={}", staged.display())]);
    r.success();

    assert!(staged.join("usr/local/lib/libcore.a").is_file(), "the archive is installed");
    assert!(staged.join("usr/local/include/core.h").is_file(), "the published header");
    assert!(staged.join("usr/local/include/core/detail.h").is_file(), "and the tree under it");
    // prefix の根は段取り用のディレクトリの下に継がれる。継がないと、
    // 段取りのつもりが本物の `/usr/local` になる。
    assert!(!std::path::Path::new("/usr/local/lib/libcore.a").exists());
}

#[test]
fn install_without_a_destination_says_which_flag_it_needs() {
    // 入れる先に既定は無い。`/usr/local` は権限を要し、書ける既定は
    // 誰の役にも立たない（ADR-0041）。
    let p = Project::new("install-no-prefix");
    p.write("dowel.toml", "[package]\nname = \"app\"\nversion = \"0\"\n");
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["install"]);
    r.failure();
    r.stderr_contains("--prefix");
}

#[test]
fn the_relative_search_path_survives_every_backend() {
    // `$ORIGIN` は make のレシピを通ると危ない。make が `$` を食い、残りを
    // シェルが変数として展開して空にする——実行ファイルは組め、ビルド木の
    // 中では動き、移した先でだけ壊れる（ADR-0041）。
    //
    // 通り道が3つある以上、3つとも通す。
    let p = Project::new("relocatable-backends");
    p.write("core/dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "core/dowel.build",
        "[lib.core]\n\
         sources = [file(\"src/core.c\")]\n\
         linkage = \"shared\"\n\
         exports = [\"core_open\"]\n\n\
         [lib.core.public]\nincludes = [dir(\"include\")]\n",
    );
    p.write("core/include/core.h", "#pragma once\nint core_open(void);\n");
    p.write("core/src/core.c", "int core_open(void) { return 42; }\n");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"core\"\npath = \"../core\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\n",
    );
    p.write(
        "app/src/main.c",
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"v=%d\\n\", core_open()); return 0; }\n",
    );

    for backend in ["ninja", "direct", "make"] {
        let _ = std::fs::remove_dir_all(p.path("app/.dowel"));
        let prefix = p.path(&format!("out-{backend}"));
        p.run(
            "app",
            &[
                "install",
                &format!("--backend={backend}"),
                &format!("--prefix={}", prefix.display()),
            ],
        )
        .success();
        // 記録された綴りをそのまま見る。走らせるだけでは、ビルド木が
        // 残っている限り絶対パスの方で解決してしまう。
        let bin = std::fs::read(prefix.join("bin/app")).expect("cannot read the executable");
        let text = String::from_utf8_lossy(&bin);
        assert!(
            text.contains("$ORIGIN/../lib"),
            "`{backend}` did not record a relative search path"
        );
        std::fs::remove_dir_all(p.path("app/.dowel")).expect("cannot remove the build tree");
        assert_eq!(run_artifact(&prefix.join("bin/app")), "v=42\n", "backend `{backend}`");
    }
}

#[test]
fn a_published_include_path_that_is_not_a_directory_is_reported_when_installing() {
    // 入れる先にヘッダが来ないことは、入れた側では気づけない——使う側の
    // ビルドが `core.h` を見つけられなくなって初めて分かる（ADR-0041）。
    let p = Project::new("install-bad-includes");
    p.write("dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.core]\nsources = [file(\"src/core.c\")]\n\n\
         [lib.core.public]\nincludes = [dir(\"nowhere\")]\n",
    );
    p.write("src/core.c", "int core_open(void) { return 42; }\n");

    let prefix = p.path("out");
    let r = p.run(".", &["install", &format!("--prefix={}", prefix.display())]);
    r.success();
    r.stderr_contains("uninstallable-headers");
    r.stderr_contains("nowhere");
    // 警告であって失敗ではない。ライブラリ自身は入る。
    assert!(prefix.join("lib/libcore.a").is_file(), "the library is still installed");
}

#[test]
fn abi_components_constrain_only_where_both_sides_name_them() {
    // 粒度を大域に1つ決めると、粗すぎれば検証が無意味になり、細かすぎれば
    // 共有が壊れる。成分ごとに比べれば、決めるのは宣言する側になる
    // （ADR-0042）。
    let p = Project::new("abi-components");
    p.write("core/dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "core/dowel.build",
        "[lib.core]\nsources = [file(\"src/core.c\")]\n\n\
         [lib.core.public]\nabi = { cxx_stdlib = \"libc++\" }\n",
    );
    p.write("core/src/core.c", "int core_open(void) { return 42; }\n");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"core\"\npath = \"../core\"\n",
    );

    // 双方が名指す成分が食い違えば落ちる。
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\n\
         abi = { cxx_stdlib = \"libstdc++\" }\n",
    );
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("abi-mismatch");
    r.stderr_contains("cxx_stdlib");

    // 片方しか名指していない成分は制約にならない。粗い側は少なく縛る。
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\n\
         abi = { libc = \"gnu\" }\n",
    );
    p.run("app", &["check"]).success();

    // 併合の結果は成分の和である。制約が途中で落ちれば、その先は検査を
    // 受けずに通る。
    let why = p.run("app", &["why", "app", "abi"]);
    why.success();
    assert!(why.stdout.contains("cxx_stdlib"), "{}", why.stdout);
    assert!(why.stdout.contains("libc"), "{}", why.stdout);
}

#[test]
fn a_label_written_as_one_word_is_still_compared_whole() {
    // 既に書かれた札を壊さない。1つの語で書かれたものは分解できないので、
    // 全体で比べる——`c` の免除もそのままである（ADR-0019）。
    let p = Project::new("abi-word");
    p.write("core/dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write(
        "core/dowel.build",
        "[lib.core]\nsources = [file(\"src/core.c\")]\n\n\
         [lib.core.public]\nabi = \"x86_64-linux-musl\"\n",
    );
    p.write("core/src/core.c", "int core_open(void) { return 42; }\n");
    p.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0\"\n\n\
         [[dependencies]]\nname = \"core\"\npath = \"../core\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\nabi = \"x86_64-linux-gnu\"\n",
    );
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("abi-mismatch");

    // 語と成分表を混ぜると比べられない。分解できないことを述べる。
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\nabi = { libc = \"musl\" }\n",
    );
    let r = p.run("app", &["check"]);
    r.failure();
    r.stderr_contains("cannot be taken apart");

    // `c` は成分表とも突き合わせない。制約を足さないだけである。
    p.write(
        "core/dowel.build",
        "[lib.core]\nsources = [file(\"src/core.c\")]\n\n\
         [lib.core.public]\nabi = \"c\"\n",
    );
    p.write(
        "app/dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"core\")]\nabi = { libc = \"gnu\" }\n",
    );
    p.run("app", &["check"]).success();
}

#[test]
fn a_surface_requiring_another_c_runtime_than_the_build_is_refused() {
    // 札同士の比較は「誰が何を要求するか」しか見ない。このビルドが何で
    // あるかは見ていない——`musl` を要求する面を gnu 向けに組めば、要求は
    // 満たされないままリンクが通り、失敗は実行時に出る（ADR-0042）。
    let p = Project::new("abi-vs-build");
    p.write("dowel.toml", "[package]\nname = \"app\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nabi = { libc = \"musl\" }\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("abi-mismatch");
    r.stderr_contains("musl");

    // 三つ組と合っていれば黙る。導ける成分だけを見るので、`cxx_stdlib` は
    // ここでは何も言われない。
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nabi = { libc = \"gnu\", cxx_stdlib = \"libc++\" }\n",
    );
    p.run(".", &["check"]).success();
}

#[test]
fn one_wrong_label_is_one_diagnostic_however_many_targets_carry_it() {
    // ビルドとの照合は**宣言と構成**の関係であり、誰が引いているかに依らない
    // ——ビルドは一様である（ADR-0031）。目標ごとに出すと、文面も位置も同じ
    // レコードが使う側の数だけ並び、「1つ直せば全部消える」のか「N 箇所直す
    // ところがある」のかが読めない（issue #158）。
    let p = Project::new("abi-vs-build-fold");
    p.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.engine]\nsources = [file(\"src/engine.c\")]\n\n\
         [lib.engine.public]\nabi = { libc = \"musl\" }\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [target(\"engine\")]\n\n\
         [bin.tool]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.tool.private]\ndeps = [target(\"engine\")]\n",
    );
    p.write("src/engine.c", "int engine_open(void) { return 1; }\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    assert_eq!(
        r.stderr.matches("error[abi-mismatch]").count(),
        1,
        "one declaration produced more than one diagnostic:\n{}",
        r.stderr
    );
    // 畳んでも失われるものが無いこと。影響の範囲は note に並ぶ。
    for label in ["`p:engine`", "`p:app`", "`p:tool`"] {
        assert!(r.stderr.contains(label), "{label} is not named as affected:\n{}", r.stderr);
    }
}

#[test]
fn a_package_that_declares_a_template_still_passes_check() {
    // `check` は何も名指ししていない。それでも `not-a-target` が出ていた
    // ——「全ターゲット」を数える経路が、雛型まで要求として計画へ渡して
    // いたためである（issue #141）。
    //
    // `build` と `test` は通っていたので、壊れていたのは機構ではなく
    // 目標の数え方の方だった。
    let p = Project::new("template-check");
    p.write("dowel.toml", "[package]\nname = \"template-check\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[template.warn]\n\n\
         [template.warn.private]\nflags = [\"-Wall\", \"-Wextra\"]\n\n\
         [bin.app]\nuse = [template(\"warn\")]\nsources = [file(\"src/main.c\")]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.success();
    assert!(!r.stderr.contains("not-a-target"), "stderr:\n{}", r.stderr);
    // 数えるのは成果物を作るものだけ。雛型は目標ではないと述べている以上、
    // 目標として数えてもいけない。
    assert!(r.stderr.contains("1 targets"), "stderr:\n{}", r.stderr);

    p.run(".", &["build"]).success();

    // 名指しは今までどおり断る。そちらは文書どおりで、有用である。
    let named = p.run(".", &["build", "warn"]);
    named.failure();
    named.stderr_contains("not-a-target");
}

#[test]
fn an_installed_library_is_found_by_pkg_config_and_builds_a_plain_consumer() {
    // dowel は `.pc` を読む側であり（ADR-0015）、書く側が無かった。結果と
    // して「ライブラリを dowel へ移すには使う側も全部同時に移す」ことに
    // なり、漸進的な導入という前提と正面から反する（ADR-0043）。
    //
    // ここで確かめるのは、dowel を一切通さない利用者が組めることである。
    // pkg-config は `version` 依存の検査が既に前提にしている。
    let p = Project::new("pkgconfig");
    p.write(
        "dowel.toml",
        "[package]\nname = \"core\"\nversion = \"1.2.3\"\n\
         description = \"a small hashing library\"\n",
    );
    p.write(
        "dowel.build",
        "[lib.core]\n\
         sources   = [file(\"src/core.c\")]\n\
         linkage   = \"shared\"\n\
         soversion = 2\n\
         exports   = [\"core_open\"]\n\n\
         [lib.core.public]\n\
         includes = [dir(\"include\")]\n\
         defines  = { CORE_SHARED = 1 }\n",
    );
    p.write("include/core.h", "#pragma once\nint core_open(void);\n");
    p.write("src/core.c", "int core_open(void) { return 42; }\n");

    let prefix = p.path("out");
    p.run(".", &["install", &format!("--prefix={}", prefix.display())]).success();

    let pc = prefix.join("lib/pkgconfig/core.pc");
    let text = std::fs::read_to_string(&pc).expect("no pkg-config file was written");
    // 記録するのは prefix であって、ビルド木でも段取り用の場所でもない。
    assert!(text.contains(&format!("prefix={}", prefix.display())), "{text}");
    // 公開の面がそのまま出ている。dowel の利用者と pkg-config の利用者が
    // 受け取るものが違ってはならない。
    assert!(text.contains("Version: 1.2.3"), "{text}");
    assert!(text.contains("Description: a small hashing library"), "{text}");
    assert!(text.contains("-DCORE_SHARED=1"), "{text}");

    let dir = prefix.join("lib/pkgconfig");
    let run = |args: &[&str]| {
        std::process::Command::new("pkg-config")
            .env("PKG_CONFIG_PATH", &dir)
            .args(args)
            .output()
            .expect("cannot start pkg-config")
    };
    assert!(run(&["--validate", "core"]).status.success(), "the file does not validate:\n{text}");

    // 印字された旗で、dowel を通さずに組んで走らせる。ここが本題である。
    let out = run(&["--cflags", "--libs", "core"]);
    assert!(out.status.success(), "pkg-config could not answer:\n{text}");
    let flags: Vec<String> =
        String::from_utf8_lossy(&out.stdout).split_whitespace().map(|s| s.to_string()).collect();
    p.write(
        "consumer.c",
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"v=%d\\n\", core_open()); return 0; }\n",
    );
    let exe = p.path("consumer");
    let built = std::process::Command::new("cc")
        .arg("-o")
        .arg(&exe)
        .arg(p.path("consumer.c"))
        .args(&flags)
        .arg(format!("-Wl,-rpath,{}", prefix.join("lib").display()))
        .output()
        .expect("cannot start cc");
    assert!(
        built.status.success(),
        "a plain consumer did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert_eq!(run_artifact(&exe), "v=42\n");
}

#[test]
fn a_library_that_sits_on_a_sibling_says_so_and_a_plain_consumer_links() {
    // 静的な書庫は自分のリンク要件を運べない。同じパッケージの下の
    // ライブラリを名指さなければ、pkg-config だけを頼りにする使う側は
    // 未定義参照を受け取る。名指してよい条件（同じ実行で書いた記述で
    // あること）は、ここでしか満たされない（issue #156、ADR-0043）。
    //
    // 静的のまま確かめる。共有にすると `DT_NEEDED` が下を連れてきて
    // しまい、記述が下を名指しているかどうかが見えない。
    let p = Project::new("pkgconfig-sibling");
    p.write(
        "dowel.toml",
        "[package]\nname = \"two\"\nversion = \"0.1.0\"\n\
         description = \"two static libraries, one on the other\"\n",
    );
    p.write(
        "dowel.build",
        "[lib.base]\nsources = [file(\"src/base.c\")]\n\n\
         [lib.base.public]\nincludes = [dir(\"a\")]\nlink_flags = [\"-lm\"]\n\n\
         [lib.top]\nsources = [file(\"src/top.c\")]\n\n\
         [lib.top.public]\nincludes = [dir(\"b\")]\n\n\
         [lib.top.private]\ndeps = [target(\"base\")]\n",
    );
    p.write("a/base.h", "#pragma once\ndouble base_area(double);\n");
    p.write("b/top.h", "#pragma once\ndouble top_area(double);\n");
    p.write(
        "src/base.c",
        "#include <math.h>\ndouble base_area(double r) { return M_PI * r * r; }\n",
    );
    p.write(
        "src/top.c",
        "double base_area(double);\ndouble top_area(double r) { return base_area(r) * 2; }\n",
    );

    let prefix = p.path("out");
    p.run(".", &["install", &format!("--prefix={}", prefix.display())]).success();

    let text = std::fs::read_to_string(prefix.join("lib/pkgconfig/top.pc")).expect("no file");
    assert!(text.contains("Requires: base"), "the descriptor does not name the sibling:\n{text}");
    // 下は誰にも乗っていない。`Requires` を持たない。
    let base = std::fs::read_to_string(prefix.join("lib/pkgconfig/base.pc")).expect("no file");
    assert!(
        !base.contains("Requires:"),
        "a library that sits on nothing requires something:\n{base}"
    );

    let dir = prefix.join("lib/pkgconfig");
    let run = |args: &[&str]| {
        std::process::Command::new("pkg-config")
            .env("PKG_CONFIG_PATH", &dir)
            .args(args)
            .output()
            .expect("cannot start pkg-config")
    };
    assert!(run(&["--validate", "top"]).status.success(), "the file does not validate:\n{text}");
    // 静的な書庫の解決順は依存元が先である。`-ltop` の後に `-lbase`。
    let libs = String::from_utf8_lossy(&run(&["--libs", "top"]).stdout).to_string();
    let top_at = libs.find("-ltop").unwrap_or_else(|| panic!("`-ltop` is missing: {libs}"));
    let base_at = libs.find("-lbase").unwrap_or_else(|| panic!("`-lbase` is missing: {libs}"));
    assert!(top_at < base_at, "the link order is inverted: {libs}");
    // 下が公表している要件も一緒に届く。`Requires` を採った理由である。
    assert!(libs.contains("-lm"), "the sibling's own link requirement did not travel: {libs}");

    // dowel を通さない利用者が組んで走る。ここが本題である。
    let out = run(&["--cflags", "--libs", "top"]);
    assert!(out.status.success(), "pkg-config could not answer:\n{text}");
    let flags: Vec<String> =
        String::from_utf8_lossy(&out.stdout).split_whitespace().map(|s| s.to_string()).collect();
    p.write(
        "consumer.c",
        "#include <stdio.h>\n#include \"top.h\"\n\
         int main(void) { printf(\"%.4f\\n\", top_area(1.0)); return 0; }\n",
    );
    let exe = p.path("consumer");
    let built = std::process::Command::new("cc")
        .arg("-o")
        .arg(&exe)
        .arg(p.path("consumer.c"))
        .args(&flags)
        .output()
        .expect("cannot start cc");
    assert!(
        built.status.success(),
        "a plain consumer did not link against the sibling: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert_eq!(run_artifact(&exe), "6.2832\n");
}

#[test]
fn a_package_without_a_description_still_writes_a_valid_file() {
    // pkg-config は `Description` を要求する。空の記述はファイルを不正に
    // するので、書かれていなければ名前で代える（ADR-0043）。
    let p = Project::new("pkgconfig-default");
    p.write("dowel.toml", "[package]\nname = \"core\"\nversion = \"0\"\n");
    p.write("dowel.build", "[lib.core]\nsources = [file(\"src/core.c\")]\n");
    p.write("src/core.c", "int core_open(void) { return 42; }\n");

    let prefix = p.path("out");
    p.run(".", &["install", &format!("--prefix={}", prefix.display())]).success();
    let text = std::fs::read_to_string(prefix.join("lib/pkgconfig/core.pc")).expect("no file");
    assert!(text.contains("Description: core"), "{text}");

    // `bin` には書かない。組んで繋ぐ相手ではない。
    assert!(!prefix.join("lib/pkgconfig/app.pc").exists());
}

/// 取ってくる道具一式（[ADR-0044](../../../docs/adr/0044-toolchain-acquisition.md)）。
///
/// 本物のクロスコンパイラは置けないので、`cc` を包むだけの薄い道具を書庫に
/// 詰める。確かめたいのは「宣言した書庫の中の道具で組んだか」であり、
/// 中身が何であるかではない。
fn toolchain_archive(p: &Project) -> (String, String) {
    p.write_script("tc/bin/mycc", "#!/bin/sh\nexec cc \"$@\"\n");
    p.write_script("tc/bin/myar", "#!/bin/sh\nexec ar \"$@\"\n");
    let archive = p.path("toolchain.tar.gz");
    let out = std::process::Command::new("tar")
        .args(["-czf", &archive.display().to_string(), "-C", &p.path(".").display().to_string()])
        .arg("tc")
        .output()
        .expect("cannot start tar");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let sha = dowel_support::sha256::hex_of_file(&archive).expect("cannot hash the archive");
    (format!("file://{}", archive.display()), sha)
}

#[test]
fn a_declared_toolchain_is_fetched_verified_and_used() {
    // マニフェストも、ソースも、依存も固定されているのに、コンパイラだけは
    // 機械に在るものだった。クロスビルドが再現するのは、object code を
    // 決める入力を除いた全て、という状態だった（ADR-0044）。
    let p = Project::new("toolchain-fetch");
    let (url, sha) = toolchain_archive(&p);
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"tc\"\nversion = \"0\"\n\n\
             [toolchain]\nurl = \"{url}\"\nsha256 = \"{sha}\"\n\
             c = \"bin/mycc\"\nar = \"bin/myar\"\n"
        ),
    );
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "#include <stdio.h>\nint main(void) { printf(\"v=1\\n\"); return 0; }\n");

    let cache = p.path("cache");
    let env = [("DOWEL_TOOLCHAIN_DIR", cache.display().to_string())];
    let envs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = p.run_env(".", &["build"], &envs);
    r.success();
    // 自分についての警告は出ない。比べるのは解いた後の綴りである。
    assert!(!r.stderr.contains("toolchain-mismatch"), "stderr:\n{}", r.stderr);

    // 使ったのは書庫の中の道具である。翻訳データベースがそれを述べる。
    let db =
        std::fs::read_to_string(build_dir(&p.path("."), "debug").join("compile_commands.json"))
            .expect("no compile database");
    assert!(db.contains("/dowel/toolchains/"), "{db}");
    assert!(db.contains("mycc"), "{db}");
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/app")), "v=1\n");

    // 木の中ではなく利用者の cache に置く。同じ書庫はどの木でも同じバイトで
    // あり、木ごとに取り直すのは最も安定したものを最も揮発しやすい場所に
    // 置くことである（ADR-0028 と同じ理屈）。
    assert!(cache.join("dowel/toolchains").is_dir(), "not in the user cache");
    assert!(!p.path(".dowel/deps").exists(), "it must not land in the tree");

    // 2度目は取りに行かない。書庫を消しても組める。
    std::fs::remove_file(p.path("toolchain.tar.gz")).expect("cannot remove the archive");
    p.run_env(".", &["build"], &envs).success();
}

#[test]
fn a_toolchain_archive_that_does_not_match_its_digest_stops_the_build() {
    // 黙って PATH の道具へ落ちてはならない。落ちると、宣言と違うコンパイラが
    // 宣言の後ろに隠れる——この決定が取り除こうとしているものそのものである。
    let p = Project::new("toolchain-digest");
    let (url, _) = toolchain_archive(&p);
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"tc\"\nversion = \"0\"\n\n\
             [toolchain]\nurl = \"{url}\"\nsha256 = \"{}\"\nc = \"bin/mycc\"\n",
            "0".repeat(64)
        ),
    );
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let cache = p.path("cache");
    let env = cache.display().to_string();
    let r = p.run_env(".", &["build"], &[("DOWEL_TOOLCHAIN_DIR", env.as_str())]);
    r.failure();
    r.stderr_contains("unfetchable-toolchain");
    r.stderr_contains("does not match its declared hash");
}

#[test]
fn a_toolchain_url_without_a_digest_is_refused() {
    // URL は名前であり、名前の裏のバイトは変わりうる（ADR-0029 と同じ）。
    let p = Project::new("toolchain-unpinned");
    let (url, _) = toolchain_archive(&p);
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"tc\"\nversion = \"0\"\n\n\
             [toolchain]\nurl = \"{url}\"\nc = \"bin/mycc\"\n"
        ),
    );
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unpinned-toolchain");
    r.stderr_contains("the bytes behind a name can change");
}

/// git 依存を1つ持つ木。取得の経路を通す検査が使う。
fn project_with_a_git_dependency(name: &str) -> (Project, String) {
    let p = Project::new(name);
    p.write("dep/dowel.toml", "[package]\nname = \"dep\"\nversion = \"0\"\n");
    p.write("dep/dowel.build", "[lib.dep]\nsources = [file(\"src/dep.c\")]\n");
    p.write("dep/src/dep.c", "int dep_answer(void) { return 42; }\n");
    let dep = p.path("dep");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dep)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("cannot start git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "."]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "x"]);
    let rev = git(&["rev-parse", "HEAD"]);

    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0\"\n\n\
             [[dependencies]]\nname = \"dep\"\ngit = \"{}\"\nrev = \"{rev}\"\n",
            dep.display()
        ),
    );
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [dep(\"dep\")]\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\nint dep_answer(void);\n\
         int main(void) { printf(\"v=%d\\n\", dep_answer()); return 0; }\n",
    );
    (p, rev)
}

#[test]
fn offline_refuses_what_is_missing_and_fetch_makes_the_tree_ready() {
    // 取得済みの木は網へ行かずに組める——偶然に。何もそう述べず、何も
    // 確かめず、そうでなくなったことも報せない（ADR-0045）。
    let (p, _) = project_with_a_git_dependency("offline");

    // 何も取っていない状態で断る。理由は「取れなかった」ではなく
    // 「取っていない」であり、直し方も違う。
    let r = p.run(".", &["build", "--offline"]);
    r.failure();
    r.stderr_contains("needs-fetch");
    r.stderr_contains("dowel fetch");
    // 網の失敗として出してはならない。何も試していない。
    assert!(!r.stderr.contains("unfetchable-dependency"), "stderr:\n{}", r.stderr);

    // 取ってくるだけで、組まない。
    let r = p.run(".", &["fetch"]);
    r.success();
    r.stderr_contains("ready: dep");
    assert!(!build_dir_exists(&p.path(".")), "`fetch` must not build");

    // これで網を切っても組める。
    let r = p.run(".", &["build", "--offline"]);
    r.success();
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/app")), "v=42\n");

    // 環境変数でも同じ。容器や CI では、命令ごとに旗を書き足すより漏れない。
    p.run_env(".", &["build"], &[("DOWEL_OFFLINE", "1")]).success();
}

#[test]
fn fetch_counts_and_lists_the_toolchain_it_acquired() {
    // 取ってくるものが道具一式だけ、という形は cross では普通である
    // （依存はすべて `path`、道具立てだけ書庫）。数にも一覧にも入れないと、
    // その木の利用者が読む唯一の行が「fetched 0 package(s)」になる。素直な
    // 解釈は「何も要らなかった」であり、数百 MB を落とした直後でも同じ行
    // である（issue #159、ADR-0045）。
    let p = Project::new("fetch-toolchain");
    let (url, sha) = toolchain_archive(&p);
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"tc\"\nversion = \"0\"\n\n\
             [toolchain]\nurl = \"{url}\"\nsha256 = \"{sha}\"\nc = \"bin/mycc\"\n"
        ),
    );
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let cache = p.path("cache").display().to_string();
    let envs = [("DOWEL_TOOLCHAIN_DIR", cache.as_str())];
    let r = p.run_env(".", &["fetch"], &envs);
    r.success();
    r.stderr_contains("ready: toolchain");
    r.stderr_contains("/dowel/toolchains/");
    assert!(
        !r.stderr.contains("0 toolchain(s)"),
        "the toolchain it acquired was not counted:\n{}",
        r.stderr
    );
    // 取ってくるだけで、組まない。
    assert!(!build_dir_exists(&p.path(".")), "`fetch` must not build");
    // 述べたとおり、これで網を切っても組める。
    p.run_env(".", &["build", "--offline"], &envs).success();
}

#[test]
fn a_toolchain_that_could_not_be_acquired_does_not_also_look_missing_from_path() {
    // 2つ目は1つ目の帰結だが、**別の直し方**を指す——「翻訳器が PATH に
    // 無い」と読めるので、翻訳器を入れに行く動機になる。ADR-0044 が
    // `missing-toolchain` に与えた役割は「取ってきたものの中に道具が
    // 無い」場合であって、取得そのものが成り立っていない場合ではない
    // （issue #159）。
    let p = Project::new("toolchain-unfetched");
    let (url, sha) = toolchain_archive(&p);
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"tc\"\nversion = \"0\"\n\n\
             [toolchain]\nurl = \"{url}\"\nsha256 = \"{sha}\"\nc = \"bin/mycc\"\n"
        ),
    );
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let cache = p.path("cache").display().to_string();
    let r = p.run_env(".", &["check", "--offline"], &[("DOWEL_TOOLCHAIN_DIR", cache.as_str())]);
    r.failure();
    r.stderr_contains("needs-fetch");
    assert!(
        !r.stderr.contains("missing-toolchain"),
        "the consequence was reported as a second, differently-fixed problem:\n{}",
        r.stderr
    );
}

/// ビルドディレクトリが1つでも在るか。`fetch` が組んでいないことを見る。
fn build_dir_exists(project_dir: &std::path::Path) -> bool {
    std::fs::read_dir(project_dir.join(".dowel/build")).is_ok_and(|mut d| d.next().is_some())
}

#[test]
fn fetch_takes_no_target_and_says_so() {
    let p = Project::new("fetch-args");
    p.write("dowel.toml", "[package]\nname = \"app\"\nversion = \"0\"\n");
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");
    let r = p.run(".", &["fetch", "app"]);
    r.failure();
    r.stderr_contains("`fetch` takes no target");
}

/// 転送を数える木（[ADR-0046](../../../docs/adr/0046-transfer-once.md)）。
///
/// 数えるのは**転送が走った回数**である。転送先の実体を見ると、
/// 「送らなかった」と「送って消えた」が同じ状態に潰れる。
fn counting_transfer_project(name: &str, launcher: &str) -> (Project, std::path::PathBuf) {
    let triple = host_triple();
    let p = Project::new(name);
    let staged = p.path("staged");
    std::fs::create_dir_all(&staged).unwrap();
    let log = p.path("transfers.log");
    p.write_script(
        "bin/copy",
        &format!("#!/bin/sh\necho sent >> {}\nexec cp \"$@\"\n", log.display()),
    );
    p.write("dowel.toml", "[package]\nname = \"r\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        &format!(
            "[test.moved]\nsources = glob(\"*.c\")\n\n\
             [runner.{triple}]\n\
             transfer   = [\"{}\"]\n\
             remote_dir = \"{}\"\n\
             command    = \"{launcher}\"\n",
            p.path("bin/copy").display(),
            staged.display()
        ),
    );
    p.write("moved.c", "int main(void) { return 0; }\n");
    (p, log)
}

#[test]
fn an_unchanged_artifact_is_not_transferred_twice() {
    // 実機やシリアル越しの転送は、実行そのものより長いことがある。
    // 変わっていないものを毎回運ぶ理由は無い（ADR-0046）。
    let (p, log) = counting_transfer_project("transfer-once", "env");
    let sent = || std::fs::read_to_string(&log).map(|t| t.lines().count()).unwrap_or(0);

    p.run(".", &["test"]).success();
    assert_eq!(sent(), 1, "the first run must transfer");

    p.run(".", &["test"]).success();
    assert_eq!(sent(), 1, "an unchanged artifact was sent again");

    // 中身が変われば送る。指紋が記録と食い違う。注釈では変わらない——
    // 記録しているのは成果物のバイト列であって、原文ではない。
    p.write("moved.c", "int main(void) { volatile int x = 7; (void)x; return 0; }\n");
    p.run(".", &["test"]).success();
    assert_eq!(sent(), 2, "a changed artifact must be sent");
}

#[test]
fn a_run_that_cannot_start_makes_the_next_one_transfer_again() {
    // 対象機の側で消された・置き換えられたことは、こちらからは見えない。
    // 見えるのは起動の失敗だけなので、それを送り直す合図に使う（ADR-0046）。
    let (p, log) = counting_transfer_project("transfer-forget", "");
    let launcher = p.path("bin/launcher");
    // 起動する道具が無い。転送は済み、走らせる側で起動に失敗する。
    p.write(
        "dowel.build",
        &std::fs::read_to_string(p.path("dowel.build"))
            .unwrap()
            .replace("command    = \"\"", &format!("command    = \"{}\"", launcher.display())),
    );
    let sent = || std::fs::read_to_string(&log).map(|t| t.lines().count()).unwrap_or(0);

    p.run(".", &["test"]).failure();
    assert_eq!(sent(), 1, "the transfer itself should have run");

    // 起動できるようにする。記録が落ちているので、中身が同じでも送り直す。
    p.write_script("bin/launcher", "#!/bin/sh\nexec \"$@\"\n");
    p.run(".", &["test"]).success();
    assert_eq!(sent(), 2, "a run that could not start must drop the record");

    // その次は、また送らない。記録が戻っている。
    p.run(".", &["test"]).success();
    assert_eq!(sent(), 2, "the record should be back");
}

#[test]
fn a_machine_that_lost_the_artifact_recovers_on_the_run_after_the_one_that_noticed() {
    // ADR-0046 が自己修復の動機に挙げた場面そのものである。板を配り直す、
    // `/tmp` が消える、誰かが片付ける——いずれも運び手は起動し、向こう側が
    // 非零で返す。`launch_error` は立たないので、起動の失敗だけを合図に
    // していると、記録は残ったまま木が失敗し続ける（issue #160）。
    let (p, log) = counting_transfer_project("transfer-vanished", "env");
    let sent = || std::fs::read_to_string(&log).map(|t| t.lines().count()).unwrap_or(0);
    let staged = p.path("staged");

    p.run(".", &["test"]).success();
    assert_eq!(sent(), 1, "the first run must transfer");

    // 対象機の側から消える。dowel からは見えない出来事である。
    std::fs::remove_file(staged.join("moved")).expect("the artifact was never transferred");

    // 気づく実行。飛ばして起動し、向こう側が非零で返す。
    p.run(".", &["test"]).failure();
    assert_eq!(sent(), 1, "this run has nothing new to send");

    // その次で直る。記録を落としてあるので、中身が同じでも送り直す。
    p.run(".", &["test"]).success();
    assert_eq!(sent(), 2, "a failed run must drop the record");
    assert!(staged.join("moved").is_file(), "the artifact did not come back");

    // 通った後は、また送らない。記録が戻っている。
    p.run(".", &["test"]).success();
    assert_eq!(sent(), 2, "the record should be back");
}

#[test]
fn sysroot_paths_resolve_against_the_declared_sysroot() {
    // `docs/30-devexp.md` 1節は `args = ["-L", sysroot()]` を載せていたが、
    // `sysroot()` は書けなかった（ADR-0047）。文書に在って実装に無い。
    //
    // 文字列連結を持たないので、`-I` と道を並べて書ける形が要る。
    // `link_flags` が先に通った道である（issue #70）。
    let p = Project::new("sysroot");
    let root = p.path("fake-sysroot");
    std::fs::create_dir_all(root.join("usr/include")).unwrap();
    p.write("fake-sysroot/usr/include/sysroot_header.h", "#define FROM_SYSROOT 1\n");
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"sr\"\nversion = \"0\"\n\n\
             [toolchain]\nsysroot = \"{}\"\n",
            root.display()
        ),
    );
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nflags = [\"-I\", sysroot(\"usr/include\")]\n",
    );
    // sysroot の中のヘッダを読めなければ組めない。解けたことが結果に出る。
    p.write(
        "src/main.c",
        "#include <sysroot_header.h>\n\
         #if !FROM_SYSROOT\n#error the sysroot include path did not resolve\n#endif\n\
         int main(void) { return 0; }\n",
    );

    p.run(".", &["build"]).success();
    let db =
        std::fs::read_to_string(build_dir(&p.path("."), "debug").join("compile_commands.json"))
            .expect("no compile database");
    assert!(db.contains(&root.join("usr/include").display().to_string()), "{db}");

    // 引数の無い `sysroot()` は根そのものを指す。継いだ末尾の区切りは付かない。
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nflags = [\"-I\", sysroot(\"usr/include\"), \"-I\", sysroot()]\n",
    );
    p.run(".", &["build"]).success();
    let db =
        std::fs::read_to_string(build_dir(&p.path("."), "debug").join("compile_commands.json"))
            .unwrap();
    assert!(db.contains(&format!("\"{}\"", root.display())), "{db}");
}

#[test]
fn a_sysroot_path_without_a_declaration_is_refused() {
    // 既定に落とさない。落とすと、指していない場所を指した命令が組み上がり、
    // 誤りはコンパイラの言葉で返ってくる（ADR-0047）。
    let p = Project::new("sysroot-missing");
    p.write("dowel.toml", "[package]\nname = \"sr\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nlink_flags = [\"-L\", sysroot(\"usr/lib\")]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("missing-sysroot");
    r.stderr_contains("declare `sysroot");
}

#[test]
fn a_fetched_toolchains_sysroot_is_found_inside_it() {
    // 相対の sysroot は、取ってきた道具一式の根から解く（ADR-0044 と同じ
    // 規則）。クロスの sysroot は、ふつうその中に在る。
    let p = Project::new("sysroot-fetched");
    let (url, sha) = toolchain_archive(&p);
    p.write("tc/sysroot/usr/include/tc_header.h", "#define FROM_TC 1\n");
    // 書庫を作り直す（ヘッダを含めるため）。
    let (url, sha) = {
        let _ = (url, sha);
        toolchain_archive(&p)
    };
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"sr\"\nversion = \"0\"\n\n\
             [toolchain]\nurl = \"{url}\"\nsha256 = \"{sha}\"\n\
             c = \"bin/mycc\"\nsysroot = \"sysroot\"\n"
        ),
    );
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\nflags = [\"-I\", sysroot(\"usr/include\")]\n",
    );
    p.write(
        "src/main.c",
        "#include <tc_header.h>\n\
         #if !FROM_TC\n#error the fetched sysroot did not resolve\n#endif\n\
         int main(void) { return 0; }\n",
    );

    let cache = p.path("cache").display().to_string();
    p.run_env(".", &["build"], &[("DOWEL_TOOLCHAIN_DIR", cache.as_str())]).success();
}

/// アセンブリを持つ木（[ADR-0048](../../../docs/adr/0048-assembly.md)）。
///
/// `.s` は前処理を通らず、`.S` は通る。両方置くのは、依存ファイルの扱いが
/// そこで分かれるためである。
fn project_with_assembly(name: &str) -> Project {
    let p = Project::new(name);
    p.write("dowel.toml", "[package]\nname = \"asm\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\"), file(\"src/add.s\"), file(\"src/mul.S\")]\n\n\
         [bin.app.private]\nc_std = \"c17\"\nc_flags = [\"-Wall\"]\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\nint asm_add(int, int);\nint asm_mul(int, int);\n\
         int main(void) { printf(\"%d %d\\n\", asm_add(2, 3), asm_mul(2, 3)); return 0; }\n",
    );
    p.write(
        "src/add.s",
        "\t.text\n\t.globl asm_add\nasm_add:\n\tmovl %edi, %eax\n\taddl %esi, %eax\n\tret\n",
    );
    p.write(
        "src/mul.S",
        "#include \"mul.h\"\n\t.text\n\t.globl NAME\nNAME:\n\tmovl %edi, %eax\n\timull %esi, %eax\n\tret\n",
    );
    p.write("src/mul.h", "#define NAME asm_mul\n");
    p
}

#[test]
fn assembly_sources_are_their_own_language() {
    // アセンブリは C の driver が組み立てるが、C ではない。`-std=c17` を
    // 手書きのアセンブリに渡すのは、言語を取り違えているだけである
    // （ADR-0048）。
    //
    // ホストが x86-64 でなければ、この綴りは組めない。
    if std::env::consts::ARCH != "x86_64" {
        return;
    }
    let p = project_with_assembly("assembly");
    let r = p.run(".", &["build"]);
    r.success();
    // 進行の表示が言語を述べる。`CC` と読めると、C の旗が掛かっていない
    // ことが説明できない。
    r.stderr_contains("AS ");
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/app")), "5 6\n");

    let db =
        std::fs::read_to_string(build_dir(&p.path("."), "debug").join("compile_commands.json"))
            .expect("no compile database");
    for line in db.lines() {
        let _ = line;
    }
    // 翻訳データベースの中で、アセンブリの行に C の旗が無いこと。
    let asm_args: Vec<&str> = db
        .split("\"file\":")
        .filter(|chunk| chunk.contains("add.s") || chunk.contains("mul.S"))
        .collect();
    assert!(!asm_args.is_empty(), "the assembly sources are not in the compile database:\n{db}");
    let whole = asm_args.join("");
    assert!(!whole.contains("-std=c17"), "a C standard reached the assembler:\n{whole}");
    // 実行可能スタックの印を付ける。手書きのアセンブリには誰も付けない。
    assert!(db.contains("-Wa,--noexecstack"), "{db}");
}

#[test]
fn a_preprocessed_assembly_source_rebuilds_when_its_header_changes() {
    // `.S` は前処理を通るので依存が在る。`.s` には無く、依存ファイルを
    // 頼んでも書かれない——宣言した出力が出ないことになる（ADR-0048）。
    if std::env::consts::ARCH != "x86_64" {
        return;
    }
    let p = project_with_assembly("assembly-deps");
    p.run(".", &["build"]).success();
    let out = build_dir(&p.path("."), "debug");
    assert!(out.join("obj/asm/app/src_mul.S.o.d").is_file(), "`.S` should have a depfile");
    assert!(!out.join("obj/asm/app/src_add.s.o.d").exists(), "`.s` has no depfile to write");

    // ヘッダを差し替えると、`.S` は組み直る。依存が繋がっている証拠である。
    p.write("src/mul.h", "#define NAME asm_mul\n#define UNUSED 1\n");
    let r = p.run(".", &["build"]);
    r.success();
    assert!(r.stderr.contains("src_mul.S.o"), "the header change did not reach it:\n{}", r.stderr);
}

#[test]
fn every_backend_builds_assembly() {
    // 依存ファイルを持たないコンパイルは、バックエンドごとに扱いが違う。
    // ninja は規則の `$depfile` を辺が束縛しないと循環として断る。
    if std::env::consts::ARCH != "x86_64" {
        return;
    }
    for backend in ["ninja", "direct", "make"] {
        let p = project_with_assembly(&format!("assembly-{backend}"));
        let r = p.run(".", &["build", &format!("--backend={backend}")]);
        r.success();
        assert_eq!(
            run_artifact(&build_dir(&p.path("."), "debug").join("bin/app")),
            "5 6\n",
            "backend `{backend}`"
        );
    }
}

#[test]
fn a_source_in_no_language_is_refused_where_it_is_declared() {
    // 通すと、C の driver が警告つきで受け取り、終了状態 0 のまま目的
    // ファイルを書かない。失敗はリンカの、ビルドディレクトリの中のパスに
    // ついての言葉になる（issue #157、ADR-0051）。
    let p = Project::new("unknown-language");
    p.write("dowel.toml", "[package]\nname = \"u\"\nversion = \"0\"\n");
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\"), file(\"src/note.txt\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");
    p.write("src/note.txt", "this is not a source\n");

    // 計画の段で言う。`check` にも出る——ビルドまで持ち越さない。
    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("unknown-source-language");
    r.stderr_contains("note.txt");
    r.stderr_contains("declared as a source here");
    assert!(!r.stderr.contains("cannot find"), "the linker was reached:\n{}", r.stderr);
}

#[test]
fn a_glob_that_sweeps_up_something_unbuildable_says_so_at_the_glob() {
    // 総当たりで拾った場合、指すべきはファイルの宣言ではなく総当たりの
    // 位置である。そこにしか書かれた行が無い（ADR-0051）。
    let p = Project::new("unknown-language-glob");
    p.write("dowel.toml", "[package]\nname = \"u\"\nversion = \"0\"\n");
    p.write("dowel.build", "[bin.app]\nsources = glob(\"src/*\")\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");
    p.write("src/README", "notes\n");

    let r = p.run(".", &["build"]);
    r.failure();
    r.stderr_contains("unknown-source-language");
    r.stderr_contains("README");
}

#[test]
fn a_tool_that_exits_zero_without_writing_its_output_is_a_failure() {
    // 現れない出力を宣言したアクションは常に古いままで、増分ビルドが
    // 収束しない（issue #157、#112 と同じ形）。バックエンドによらず、
    // 「built:」と刷る前に在ることを確かめる（ADR-0051）。
    let p = Project::new("silent-tool");
    let cc = p.write_script("bin/silent-cc", "#!/bin/sh\nexit 0\n");
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"s\"\nversion = \"0\"\n\n[toolchain]\nc = \"{}\"\n",
            cc.display()
        ),
    );
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/main.c\")]\n");
    p.write("src/main.c", "int main(void) { return 0; }\n");

    for backend in ["ninja", "direct", "make"] {
        let _ = std::fs::remove_dir_all(p.path(".dowel"));
        let r = p.run(".", &["build", &format!("--backend={backend}")]);
        r.failure();
        assert!(
            !r.stderr.contains("built:"),
            "backend `{backend}` reported an artifact that is not there:\n{}",
            r.stderr
        );
        // 直接実行はアクションを自分で起こすので、どの翻訳が黙ったかまで
        // 言える。ninja と make は道具を起こす側ではないので、言えるのは
        // 成果物が無いことだけである（ADR-0051）。
        if backend == "direct" {
            assert!(
                r.stderr.contains("src_main.c.o"),
                "the failure does not name the object that was not written:\n{}",
                r.stderr
            );
        }
    }
}

/// 別に宣言されたアセンブラを持つ木
/// （[ADR-0050](../../../docs/adr/0050-separate-assembler.md)）。
///
/// nasm も ml64 も置けないので、`-f <形式> -o <出力> <入力>` を受け取って
/// gas に流し直す偽物を置く。確かめたいのは「dowel が宣言された道具を、
/// 組み立てた引数で起こすか」であって、その道具が何であるかではない。
/// 受け取った引数はそのまま記録して、渡っていないものも見えるようにする。
fn project_with_declared_assembler(name: &str) -> (Project, std::path::PathBuf) {
    let p = Project::new(name);
    let log = p.path("asm-argv.txt");
    p.write_script(
        "bin/fake-nasm",
        &format!(
            "#!/bin/sh\necho \"$@\" >> {}\n\
             obj=\"\"; src=\"\"\n\
             while [ $# -gt 0 ]; do\n\
             case \"$1\" in\n\
             -f) shift 2 ;;\n\
             -o) obj=\"$2\"; shift 2 ;;\n\
             *) src=\"$1\"; shift ;;\n\
             esac\n\
             done\n\
             exec cc -x assembler -c \"$src\" -o \"$obj\"\n",
            log.display()
        ),
    );
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"masm\"\nversion = \"0\"\n\n[toolchain]\nasm = \"{}\"\n",
            p.path("bin/fake-nasm").display()
        ),
    );
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\"), file(\"src/kernel.asm\")]\n\n\
         [bin.app.private]\nc_std = \"c17\"\nc_flags = [\"-Wall\"]\n\
         asm_flags = [\"-f\", \"elf64\"]\nincludes = [dir(\"include\")]\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\n#include \"answer.h\"\nint kernel_answer(void);\n\
         int main(void) { printf(\"%d\\n\", kernel_answer() + ANSWER_BASE); return 0; }\n",
    );
    p.write("include/answer.h", "#define ANSWER_BASE 35\n");
    // 偽のアセンブラは gas へ流すので、綴りは gas のものである。拡張子が
    // 決めるのは道具であって、その道具が読む構文ではない。
    p.write(
        "src/kernel.asm",
        "\t.text\n\t.globl kernel_answer\nkernel_answer:\n\tmovl $7, %eax\n\tret\n",
    );
    (p, log)
}

#[test]
fn a_declared_assembler_gets_the_assembly_and_only_its_own_flags() {
    // アセンブリは宣言された道具へ行き、渡るのは入出力と `asm_flags` だけ
    // である。翻訳の行の残りは C の driver の綴りであり、アセンブラは
    // driver ではない（ADR-0050）。
    if std::env::consts::ARCH != "x86_64" {
        return;
    }
    let (p, log) = project_with_declared_assembler("declared-assembler");
    let r = p.run(".", &["build"]);
    r.success();
    r.stderr_contains("AS ");
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/app")), "42\n");

    let argv = std::fs::read_to_string(&log).expect("the declared assembler never ran");
    assert!(argv.contains("-f elf64"), "`asm_flags` did not reach the assembler: {argv}");
    assert!(argv.contains("kernel.asm"), "the source did not reach the assembler: {argv}");
    // C の driver の綴りは渡らない。読めるものが1つでも在れば、
    // 「アセンブラは driver ではない」という判断が実装に無い。
    // 語で見る——引数の中身ではなく引数そのものを検める。
    let words: Vec<&str> = argv.split_whitespace().collect();
    for spelling in ["-std=c17", "-Wall", "-g", "-O0", "-MD", "-c"] {
        assert!(!words.contains(&spelling), "`{spelling}` reached the assembler: {argv}");
    }
    assert!(
        !words.iter().any(|w| w.starts_with("-I")),
        "an include path reached the assembler: {argv}"
    );
    // 依存ファイルも頼まない。頼み方が道具ごとに違う。
    let out = build_dir(&p.path("."), "debug");
    assert!(!out.join("obj/masm/app/src_kernel.asm.o.d").exists(), "a depfile was declared");
}

#[test]
fn assembly_the_c_driver_cannot_take_says_which_declaration_is_missing() {
    // `.asm` は MASM / NASM の構文であり、driver は受け取れない。宣言が
    // 無ければ、リンカの「形式が分からない」より前にそう述べる（ADR-0050）。
    let p = Project::new("masm-without-assembler");
    p.write("dowel.toml", "[package]\nname = \"masm\"\nversion = \"0\"\n");
    p.write("dowel.build", "[bin.app]\nsources = [file(\"src/kernel.asm\")]\n");
    p.write("src/kernel.asm", "\t.text\n\t.globl kernel_answer\nkernel_answer:\n\tret\n");

    let r = p.run(".", &["build"]);
    r.failure();
    r.stderr_contains("missing-assembler");
    r.stderr_contains("asm = \"nasm\"");
}

#[test]
fn a_source_that_cannot_be_built_is_underlined_where_it_is_written() {
    // 「このソースはここでは組めない」に答える診断は2つある——アセンブラが
    // 無い `.asm`（ADR-0050）と、そもそも言語でない綴り（ADR-0051）。
    // 対になる以上、指す位置も揃っていなければならない。片方が目標の見出しを
    // 指していた（issue #172）: 註は「declared as a source here」と言うのに、
    // そこにソースは書かれていない。30 のソースを持つ目標では、どれが問題か
    // を本文の文字列から探すことになり、編集器からはその行へ飛べない。
    let p = Project::new("source-underline");
    p.write("dowel.toml", "[package]\nname = \"u\"\nversion = \"0\"\n");
    let sources =
        "sources = [file(\"src/main.c\"), file(\"src/five.asm\"), file(\"src/note.txt\")]";
    p.write("dowel.build", &format!("[bin.app]\n{sources}\n"));
    p.write("src/main.c", "int main(void) { return 0; }\n");
    p.write("src/five.asm", "\t.text\n");
    p.write("src/note.txt", "not a source\n");

    let r = p.run(".", &["check", "--message-format=json"]);
    r.failure();
    // 期待する列は、その要素が書かれた位置そのものである。診断の側から
    // 導かず、原文を数えて突き合わせる。
    let column_of = |needle: &str| sources.find(needle).expect("the fixture changed") + 1;
    let reported = |code: &str| -> (u64, u64) {
        let line = r
            .stdout
            .lines()
            .chain(r.stderr.lines())
            .find(|l| l.contains(&format!("\"code\":\"{code}\"")))
            .unwrap_or_else(|| panic!("no {code} diagnostic:\n{}\n{}", r.stdout, r.stderr));
        let json = dowel_support::json::parse(line).expect("the diagnostic is not JSON");
        let label = &json.get("labels").and_then(|l| l.as_array()).expect("no labels")[0];
        let at = |k: &str| {
            label.get(k).and_then(|v| v.as_f64()).unwrap_or_else(|| panic!("no {k}")) as u64
        };
        (at("line"), at("column"))
    };

    // 2行目が `sources = ...` である。どちらも自分の `file(...)` を指す。
    assert_eq!(reported("missing-assembler"), (2, column_of("file(\"src/five.asm\")") as u64));
    assert_eq!(
        reported("unknown-source-language"),
        (2, column_of("file(\"src/note.txt\")") as u64)
    );
}

#[test]
fn objects_from_a_declared_assembler_do_not_ask_for_an_executable_stack() {
    // 別のアセンブラの出力に `.note.GNU-stack` は無く、dowel はその道具の
    // 綴りで印を頼めない。リンカの綴りは知っているので、そこで断る
    // （ADR-0050）。リンクしない木にまで掛けないことも同時に確かめる。
    if std::env::consts::ARCH != "x86_64" {
        return;
    }
    let (p, _log) = project_with_declared_assembler("assembler-stack");
    let link_log = p.path("link-argv.txt");
    p.write_script(
        "bin/rec-link",
        &format!("#!/bin/sh\necho \"$@\" >> {}\nexec cc \"$@\"\n", link_log.display()),
    );
    p.write(
        "dowel.toml",
        &format!(
            "[package]\nname = \"masm\"\nversion = \"0\"\n\n[toolchain]\nasm  = \"{}\"\nlink = \"{}\"\n",
            p.path("bin/fake-nasm").display(),
            p.path("bin/rec-link").display()
        ),
    );
    // アセンブリを持たない実行ファイルを並べる。印の無い目的コードが
    // 閉包に居ないので、リンクに何も足らない。
    p.write(
        "dowel.build",
        "[bin.app]\nsources = [file(\"src/main.c\"), file(\"src/kernel.asm\")]\n\n\
         [bin.app.private]\nasm_flags = [\"-f\", \"elf64\"]\nincludes = [dir(\"include\")]\n\n\
         [bin.plain]\nsources = [file(\"src/plain.c\")]\n",
    );
    p.write("src/plain.c", "int main(void) { return 0; }\n");

    p.run(".", &["build"]).success();
    let argv = std::fs::read_to_string(&link_log).expect("the declared linker never ran");
    let line_for = |name: &str| -> String {
        argv.lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("`{name}` was not linked:\n{argv}"))
            .to_string()
    };
    let app = line_for("bin/app");
    assert!(app.contains("-z noexecstack"), "the assembled objects were left executable: {app}");
    let plain = line_for("bin/plain");
    assert!(!plain.contains("noexecstack"), "a build with no such objects was touched: {plain}");
}

/// 他のビルドシステムが作った静的ライブラリを模す
/// （[ADR-0049](../../../docs/adr/0049-prebuilt-libraries.md)）。
///
/// cargo も zig も go も置けないので、`cc` と `ar` で同じ形のものを作る。
/// 確かめたいのは「dowel が組まなかった書庫に繋げるか」であって、
/// それを誰が作ったかではない。
fn prebuilt_archive(p: &Project) {
    p.write("vendor/engine.c", "int engine_answer(void) { return 42; }\n");
    p.write("vendor/include/engine.h", "#pragma once\nint engine_answer(void);\n");
    let dir = p.path("vendor");
    let run = |program: &str, args: &[&str]| {
        let out = std::process::Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap_or_else(|e| panic!("cannot start {program}: {e}"));
        assert!(out.status.success(), "{program}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run("cc", &["-c", "engine.c", "-o", "engine.o"]);
    run("ar", &["rcs", "libengine.a", "engine.o"]);
}

#[test]
fn a_prebuilt_library_is_an_ordinary_dependency() {
    // 他の道具が作った書庫に繋ぐ綴りは `link_flags` にパスを直書きする以外に
    // なかった。それは依存ではないので、面も伝わらず、ABI 札も付かず、
    // `dowel why` にも出ない（ADR-0049）。
    let p = Project::new("prebuilt");
    prebuilt_archive(&p);
    p.write("dowel.toml", "[package]\nname = \"pre\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.engine]\nprebuilt = file(\"vendor/libengine.a\")\n\n\
         [lib.engine.public]\nincludes = [dir(\"vendor/include\")]\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [target(\"engine\")]\n",
    );
    p.write(
        "src/main.c",
        "#include <stdio.h>\n#include \"engine.h\"\n\
         int main(void) { printf(\"v=%d\\n\", engine_answer()); return 0; }\n",
    );

    let r = p.run(".", &["build"]);
    r.success();
    // 組むものは無い。書庫作成も走らない。
    assert!(!r.stderr.contains("AR "), "a prebuilt library must not be archived:\n{}", r.stderr);
    assert_eq!(run_artifact(&build_dir(&p.path("."), "debug").join("bin/app")), "v=42\n");

    // 面は普通に伝わる。`link_flags` の直書きにはできなかったことである。
    let why = p.run(".", &["why", "app", "includes"]);
    why.success();
    assert!(why.stdout.contains("engine"), "{}", why.stdout);
}

#[test]
fn a_prebuilt_library_carries_an_abi_label_that_is_checked() {
    // この検査は「片方がここで組まれていない」場合のために設計されている
    // （ADR-0042）。そういう相手が持てるようになったのは今回である。
    let p = Project::new("prebuilt-abi");
    prebuilt_archive(&p);
    p.write("dowel.toml", "[package]\nname = \"pre\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.engine]\nprebuilt = file(\"vendor/libengine.a\")\n\n\
         [lib.engine.public]\nabi = { libc = \"musl\" }\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [target(\"engine\")]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("abi-mismatch");
    r.stderr_contains("musl");
}

#[test]
fn a_prebuilt_library_that_is_not_there_says_so_before_linking() {
    // 無いまま進むと、リンカの言葉で1段あとに現れる（issue #50 と同じ理由）。
    let p = Project::new("prebuilt-missing");
    p.write("dowel.toml", "[package]\nname = \"pre\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.engine]\nprebuilt = file(\"vendor/libengine.a\")\n\n\
         [bin.app]\nsources = [file(\"src/main.c\")]\n\n\
         [bin.app.private]\ndeps = [target(\"engine\")]\n",
    );
    p.write("src/main.c", "int main(void) { return 0; }\n");

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("missing-prebuilt");
    // 誰が作るはずだったかを述べる。dowel はそのビルドを走らせない。
    r.stderr_contains("does not run the build that produces it");
}

#[test]
fn a_target_cannot_be_both_built_here_and_built_elsewhere() {
    let p = Project::new("prebuilt-both");
    prebuilt_archive(&p);
    p.write("dowel.toml", "[package]\nname = \"pre\"\nversion = \"0\"\n");
    p.write(
        "dowel.build",
        "[lib.engine]\nsources = [file(\"vendor/engine.c\")]\n\
         prebuilt = file(\"vendor/libengine.a\")\n",
    );

    let r = p.run(".", &["check"]);
    r.failure();
    r.stderr_contains("prebuilt-with-sources");
}
