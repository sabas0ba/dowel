//! e2e テスト用の一時プロジェクトと `dowel` の起動。

#![allow(dead_code)]
// テストバイナリごとに別々にコンパイルされるため、
// 一部のバイナリからしか使わない補助は未使用に見える。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct Project {
    pub root: PathBuf,
}

impl Project {
    pub fn new(name: &str) -> Project {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = workspace_target().join("e2e").join(format!("{name}-{n}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("cannot create the scratch directory");
        Project { root }
    }

    pub fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("cannot create the parent directory");
        }
        std::fs::write(&path, contents).expect("cannot write the file");
        path
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// `dowel` を `dir`（プロジェクトからの相対）で起動する。
    pub fn run(&self, dir: &str, args: &[&str]) -> Run {
        self.run_env(dir, args, &[])
    }

    /// 環境変数を与えて起動する。pkg-config の探索先（`PKG_CONFIG_PATH`）等、
    /// 外部委譲の検査で要る。
    pub fn run_env(&self, dir: &str, args: &[&str], envs: &[(&str, &str)]) -> Run {
        let cwd = self.root.join(dir);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dowel"));
        cmd.args(args)
            .current_dir(&cwd)
            // ログ水準を環境から漏らさない。テストの出力を安定させる。
            .env_remove("DOWEL_LOG");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("cannot start dowel");
        Run::new(args, out)
    }
}

pub struct Run {
    pub args: String,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    fn new(args: &[&str], out: Output) -> Run {
        Run {
            args: args.join(" "),
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }
    }

    pub fn success(&self) -> &Run {
        assert_eq!(self.status, Some(0), "`dowel {}` failed\n{self}", self.args);
        self
    }

    pub fn failure(&self) -> &Run {
        assert_ne!(self.status, Some(0), "`dowel {}` unexpectedly succeeded\n{self}", self.args);
        self
    }

    pub fn stderr_contains(&self, needle: &str) -> &Run {
        assert!(self.stderr.contains(needle), "stderr does not contain `{needle}`\n{self}");
        self
    }

    pub fn stdout_contains(&self, needle: &str) -> &Run {
        assert!(self.stdout.contains(needle), "stdout does not contain `{needle}`\n{self}");
        self
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "--- exit status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.status, self.stdout, self.stderr
        )
    }
}

/// ディレクトリを再帰的に複製する。過去のビルド結果は持ち込まない。
///
/// リポジトリに置いた現物（`examples/`、`tests/projects/`）は汚さず、
/// `target/` 配下へ写してからビルドする。
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("cannot create the destination directory");
    for entry in std::fs::read_dir(from).expect("cannot read the source directory").flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if entry.file_name() == ".dowel" {
                continue;
            }
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("cannot copy the file");
        }
    }
}

/// リポジトリのルート。現物を写す元を指すために使う。
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").to_path_buf()
}

/// 成果物を実行して標準出力を返す。
pub fn run_artifact(path: &Path) -> String {
    let out = Command::new(path)
        .output()
        .unwrap_or_else(|e| panic!("cannot start {}: {e}", path.display()));
    assert!(out.status.success(), "{} exited abnormally: {:?}", path.display(), out.status);
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `.dowel/build/<構成>/` を1つ見つける。構成識別子はホストに依存するため。
pub fn build_dir(project_dir: &Path, opt: &str) -> PathBuf {
    let base = project_dir.join(".dowel/build");
    let entries =
        std::fs::read_dir(&base).unwrap_or_else(|e| panic!("cannot read {}: {e}", base.display()));
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(opt) {
            return e.path();
        }
    }
    panic!("no `{opt}` build directory under {}", base.display());
}

fn workspace_target() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").to_path_buf()
}
