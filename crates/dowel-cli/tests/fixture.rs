//! 実物のプロジェクトを丸ごとビルドする層。
//!
//! 合成した2パッケージのプロジェクトは、意味論を1つずつ切り出すには良いが、
//! 現実のプロジェクトが持つ形（3層以上の依存、ダイヤモンド、公開と非公開の混在、
//! 全パッケージが公開ヘッダを `include/` に置く慣習）を持たない。
//! そこでしか現れない欠陥があるため、現物を `tests/projects/` に置いて丸ごと通す。
//!
//! 主張の大半はフィクスチャ側の C に書いてある（`#error` と終了状態）。
//! ここに書くのは、C から観測できないものだけである。
//! 規約は `tests/projects/README.md`、設計は `docs/51-testing.md` にある。

mod common;

use common::{build_dir, copy_dir, repo_root, run_artifact, Project};
use std::path::{Path, PathBuf};

/// このファイルがテストを持っているフィクスチャ。
///
/// `tests/projects/` に置いただけでテストが増えるようにはしていない。
/// フィクスチャごとに「C から観測できない主張」が違うためである。
/// 置き忘れは [`every_fixture_directory_has_a_test`] が検出する。
const FIXTURES: &[&str] = &["layered"];

fn stage(name: &str) -> Project {
    let p = Project::new(&format!("fixture-{name}"));
    copy_dir(&repo_root().join("tests/projects").join(name), &p.root);
    p
}

/// フィクスチャ内のパッケージ（`dowel.toml` を持つ直下のディレクトリ）。辞書順。
fn packages(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root)
        .expect("cannot read the fixture directory")
        .flatten()
        .filter(|e| e.path().join("dowel.toml").exists())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    out
}

/// 全パッケージについて `check` → `build` → `build`（何も走らないこと）→ `test`。
///
/// 2回目のビルドを見るのは、無駄な再実行が「ただ遅いだけ」に見えて
/// 実際には依存の取りこぼしの裏返しであることが多いためである。
fn check_build_and_test(p: &Project) {
    for pkg in packages(&p.root) {
        p.run(&pkg, &["check"]).success();
        p.run(&pkg, &["build"]).success();

        let again = p.run(&pkg, &["build", "--executor=direct", "--log-level=debug"]);
        again.success().stderr_contains("ran 0 actions");

        p.run(&pkg, &["test"]).success().stderr_contains("0 failed");
    }
}

#[test]
fn every_fixture_directory_has_a_test() {
    let dir = repo_root().join("tests/projects");
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    found.sort();
    let mut known: Vec<String> = FIXTURES.iter().map(|s| s.to_string()).collect();
    known.sort();
    assert_eq!(
        found, known,
        "a fixture directory has no test in this file (or the other way round). \
         see tests/projects/README.md"
    );
}

#[test]
fn every_fixture_is_left_clean_in_the_repository() {
    // ハーネスを通さずに実行した跡が現物に残っていないこと。
    // 残っていると、次の実行が前回の結果を引き継いで通ってしまう。
    for name in FIXTURES {
        let dir = repo_root().join("tests/projects").join(name);
        for pkg in packages(&dir) {
            for stray in [".dowel", "compile_commands.json"] {
                let path = dir.join(&pkg).join(stray);
                assert!(
                    !path.exists(),
                    "{} is a build artifact and must not be in the repository",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn layered_builds_runs_and_tests() {
    let p = stage("layered");
    check_build_and_test(&p);

    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 encode=5 port=1024 opt=0\n");

    // 構成を変えても通り、実行結果に反映される。
    p.run("app", &["build", "--config=release"]).success();
    let bin = build_dir(&p.path("app"), "release").join("bin/app");
    assert_eq!(run_artifact(&bin), "sum=5 encode=5 port=1024 opt=1\n");
}

/// `app/src/main.c` に対するコンパイル引数。`compile_commands.json` から取る。
///
/// 実際にコンパイラへ渡る引数そのものであり、伝播の結果を最も直接に映す。
fn app_arguments(p: &Project) -> String {
    let path = p.path("app/compile_commands.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    // 整形済みの JSON なので、項目の区切りで切って目当ての1件を取る。
    text.split("\n  {")
        .find(|entry| entry.contains("/app/src/main.c"))
        .unwrap_or_else(|| panic!("no entry for app/src/main.c in {}", path.display()))
        .to_string()
}

#[test]
fn layered_resolves_transitive_includes_to_distinct_directories() {
    // 5パッケージ全てが公開ヘッダを `include/` に置く。相対パスだけで
    // 重複を判定すると1つを残して消える。C 側では「include できたか」しか
    // 見えないため、実際に別々のディレクトリが並んでいることはここで確かめる。
    let p = stage("layered");
    p.run("app", &["build"]).success();
    let args = app_arguments(&p);

    for pkg in ["base", "codec", "net"] {
        let expected = format!("{pkg}/include");
        assert!(args.contains(&expected), "`{expected}` is missing from the compile arguments");
    }
}

#[test]
fn layered_does_not_leak_a_private_dependency_into_dependents() {
    // `util` は `net` の非公開依存。`app` の引数に現れてはならない。
    // C 側は「定義が無いこと」を見るが、探索パスの漏れはここでしか見えない。
    let p = stage("layered");
    p.run("app", &["build"]).success();
    let args = app_arguments(&p);
    assert!(!args.contains("util/include"), "a private dependency leaked:\n{args}");
    assert!(!args.contains("UTIL_API"), "a private dependency leaked:\n{args}");
    assert!(!args.contains("NET_INTERNAL"), "a private define leaked:\n{args}");
}

#[test]
fn layered_shows_the_diamond_in_the_graph() {
    let p = stage("layered");
    let r = p.run("app", &["graph", "--format=dot"]);
    r.success();
    // base へ2経路。片方だけになっていたら形が崩れている。
    r.stdout_contains("codec:codec");
    r.stdout_contains("net:net");
    for from in ["codec:codec", "net:net"] {
        assert!(
            r.stdout.lines().any(|l| l.contains(from) && l.contains("base:base")),
            "the edge {from} -> base:base is missing\n{}",
            r.stdout
        );
    }
}

#[test]
fn layered_explains_where_a_transitive_define_came_from() {
    // `dowel why` が2段の伝播を辿れること。デバッグの主要な導線であり、
    // 経路が1段しかない合成プロジェクトでは検査にならない。
    let p = stage("layered");
    let r = p.run("app", &["why", "app:app", "defines"]);
    r.success();
    r.stdout_contains("base:base");
    r.stdout_contains("BASE_API");
}

#[test]
fn layered_writes_a_compile_command_for_every_source() {
    let p = stage("layered");
    p.run("app", &["build"]).success();
    let path: PathBuf = p.path("app/compile_commands.json");
    let text = std::fs::read_to_string(&path).expect("cannot read compile_commands.json");
    // 5パッケージ分のライブラリソース＋実行ファイル＋各テスト。
    for source in ["base.c", "codec.c", "net.c", "util.c", "main.c", "wiring_test.c"] {
        assert!(text.contains(source), "`{source}` is missing from compile_commands.json");
    }
}
