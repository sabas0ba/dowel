//! 実プロジェクトの形状を持つフィクスチャをビルドする層。
//!
//! 合成した2パッケージのプロジェクトは、意味論を個別に検査する用途には適するが、
//! 実プロジェクトの依存形状（3層以上の依存、ダイヤモンド、公開と非公開の混在、
//! 全パッケージが公開ヘッダを `include/` に置く慣習）を持たない。
//! この形状でのみ発現する欠陥があるため、実体を `tests/projects/` に置いて検査する。
//!
//! 検査の大半はフィクスチャ側の C に記述する（`#error` と終了状態）。
//! 本ファイルに記述するのは、C から観測できない項目のみである。
//! 規約は `tests/projects/README.md`、設計は `docs/51-testing.md` にある。

mod common;

use common::{build_dir, copy_dir, repo_root, run_artifact, Project};
use std::path::{Path, PathBuf};

/// このファイルがテストを持っているフィクスチャ。
///
/// `tests/projects/` への配置のみではテストは増えない。C から観測できない
/// 検査項目がフィクスチャごとに異なるためである。
/// 未記載は [`every_fixture_directory_has_a_test`] が検出する。
const FIXTURES: &[&str] = &["configured", "layered"];

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
/// 2回目のビルドを検査するのは、不要な再実行が依存関係の記述漏れを
/// 示している場合があるためである。
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
    // ハーネスを経由せずに実行した成果物が実体に残っていないこと。
    // 残っている場合、次の実行が前回の結果を引き継いで成功する。
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

/// 構成を指定してビルドし、`app` を実行した1行を返す。
///
/// ビルドディレクトリ名は構成識別子そのものである（`<トリプル>-<opt>[-<機能>]`）。
/// 機能の並びは辞書順で、`Config::id` が決める。
fn built_config(p: &Project, args: &[&str], id_suffix: &str) -> String {
    p.run("app", &[&["build"], args].concat()).success();
    let bin = build_dir(&p.path("app"), id_suffix).join("bin/app");
    run_artifact(&bin).trim_end().to_string()
}

#[test]
fn configured_reflects_every_configuration() {
    // `match` と後置 `when` と機能フラグの組み合わせは、単一の構成では
    // 1本の経路しか通らない。切り替えて初めて分岐の全体が確かめられる。
    let p = stage("configured");
    check_build_and_test(&p);

    // 既定。`default = ["fast"]` で、`fast` が `simd` を連鎖して有効にする。
    assert_eq!(built_config(&p, &[], "debug-fast+simd"), "opt=0 fast=1 simd=1 trace=0 json=0");
    // `match cfg.opt` の別のアーム。
    assert_eq!(
        built_config(&p, &["--config=release"], "release-fast+simd"),
        "opt=1 fast=1 simd=1 trace=0 json=0"
    );
    // 既定を切ると連鎖ごと消える。
    assert_eq!(
        built_config(&p, &["--no-default-features"], "-debug"),
        "opt=0 fast=0 simd=0 trace=0 json=0"
    );
    // 明示した機能は既定に加わる。
    assert_eq!(
        built_config(&p, &["--features=trace"], "debug-fast+simd+trace"),
        "opt=0 fast=1 simd=1 trace=1 json=0"
    );
    // 任意の依存を有効にすると、その公開定義が依存元へ届く。
    assert_eq!(
        built_config(&p, &["--features=json"], "debug-fast+json+simd"),
        "opt=0 fast=1 simd=1 trace=0 json=1"
    );
}

#[test]
fn configured_does_not_read_an_optional_dependency_that_is_off() {
    // 既定では `json` は無効である。読み込んでいれば依存グラフに現れる。
    let p = stage("configured");
    let off = p.run("app", &["graph"]);
    off.success();
    assert!(!off.stdout.contains("json:json"), "the disabled package is a node\n{off}");

    let on = p.run("app", &["graph", "--features=json"]);
    on.success().stdout_contains("json:json");
}

#[test]
fn configured_keeps_a_match_arm_written_inside_a_list() {
    // 列の要素に `match` を書くと、具体化した結果は列の中の列になる。
    // 1段しか解かないと、`check` も `why` も通るのにコンパイル引数にだけ
    // 現れないという状態になる。
    let p = stage("configured");
    p.run("app", &["build"]).success();
    let path = p.path("app/compile_commands.json");
    let text = std::fs::read_to_string(&path).expect("cannot read compile_commands.json");
    assert!(text.contains("-DAPP_OPT=0"), "the nested match arm never reached the compiler");
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
    // 5パッケージ全てが公開ヘッダを `include/` に置く。相対パスのみで
    // 重複を判定すると1つを残して除去される。C 側では include の成否しか
    // 観測できないため、探索パスの内容はここで検査する。
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
    // `util` は `net` の非公開依存であり、`app` の引数に現れてはならない。
    // C 側は定義の不在を検査するが、探索パスの漏れはここでのみ観測できる。
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
    // base へ2経路。片方のみの場合、依存形状が正しく構築されていない。
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
    // `dowel why` が2段の伝播を辿れること。経路が1段の合成プロジェクトでは
    // この性質を検査できない。
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
