//! `examples/hello` が実際にビルドできることの検査。
//!
//! 例は文書の一部であり、腐ると害になる。生成した一時プロジェクトではなく
//! リポジトリに置いた現物をビルドすることで、構文や意味論を変えたときに
//! 例の更新漏れが検出される。
//!
//! 例のディレクトリ自体は汚さない。`target/` 配下へ複製してからビルドする。

mod common;

use common::{build_dir, run_artifact, Project};
use std::path::Path;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("cannot create the destination directory");
    for entry in std::fs::read_dir(from).expect("cannot read the source directory").flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            // 過去のビルド結果は持ち込まない。
            if entry.file_name() == ".dowel" {
                continue;
            }
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("cannot copy the file");
        }
    }
}

fn staged_example() -> Project {
    let p = Project::new("example-hello");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello");
    copy_dir(&source, &p.root);
    p
}

#[test]
fn the_example_builds_and_runs() {
    let p = staged_example();
    p.run("app", &["check"]).success();
    p.run("app", &["build"]).success();

    let bin = build_dir(&p.path("app"), "debug").join("bin/app");
    assert_eq!(run_artifact(&bin), "hello from libgreet (opt=0 api=1)\n");

    p.run("app", &["build", "--config=release"]).success();
    let bin = build_dir(&p.path("app"), "release").join("bin/app");
    assert_eq!(run_artifact(&bin), "hello from libgreet (opt=1 api=1)\n");
}

#[test]
fn the_example_tests_pass() {
    let p = staged_example();
    let r = p.run("libgreet", &["test"]);
    r.success();
    r.stderr_contains("test libgreet:greet_test ... ok");
    r.stderr_contains("test result: ok. 1 passed; 0 failed");
}

#[test]
fn the_commands_in_the_example_readme_work() {
    let p = staged_example();
    p.run("app", &["why", "app:app", "includes"])
        .success()
        .stdout_contains("includes of libgreet:greet");
    p.run("app", &["graph", "--kind=action"]).success().stdout_contains("LINK ");
}
