//! e2e テスト用の一時プロジェクトと `dowel` の起動。

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
        std::fs::create_dir_all(&root).expect("一時ディレクトリを作れない");
        Project { root }
    }

    pub fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("親ディレクトリを作れない");
        }
        std::fs::write(&path, contents).expect("書き込めない");
        path
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// `dowel` を `dir`（プロジェクトからの相対）で起動する。
    pub fn run(&self, dir: &str, args: &[&str]) -> Run {
        let cwd = self.root.join(dir);
        let out = Command::new(env!("CARGO_BIN_EXE_dowel"))
            .args(args)
            .current_dir(&cwd)
            // ログ水準を環境から漏らさない。テストの出力を安定させる。
            .env_remove("DOWEL_LOG")
            .output()
            .expect("dowel を起動できない");
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
        assert_eq!(self.status, Some(0), "`dowel {}` が失敗した\n{self}", self.args);
        self
    }

    pub fn failure(&self) -> &Run {
        assert_ne!(self.status, Some(0), "`dowel {}` が成功してしまった\n{self}", self.args);
        self
    }

    pub fn stderr_contains(&self, needle: &str) -> &Run {
        assert!(self.stderr.contains(needle), "stderr に `{needle}` がない\n{self}");
        self
    }

    pub fn stdout_contains(&self, needle: &str) -> &Run {
        assert!(self.stdout.contains(needle), "stdout に `{needle}` がない\n{self}");
        self
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "--- 終了状態: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.status, self.stdout, self.stderr
        )
    }
}

/// 成果物を実行して標準出力を返す。
pub fn run_artifact(path: &Path) -> String {
    let out = Command::new(path)
        .output()
        .unwrap_or_else(|e| panic!("{} を起動できない: {e}", path.display()));
    assert!(out.status.success(), "{} が異常終了した: {:?}", path.display(), out.status);
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `.dowel/build/<構成>/` を1つ見つける。構成識別子はホストに依存するため。
pub fn build_dir(project_dir: &Path, opt: &str) -> PathBuf {
    let base = project_dir.join(".dowel/build");
    let entries =
        std::fs::read_dir(&base).unwrap_or_else(|e| panic!("{} を読めない: {e}", base.display()));
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(opt) {
            return e.path();
        }
    }
    panic!("{} に `{opt}` 構成のビルドディレクトリがない", base.display());
}

fn workspace_target() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").to_path_buf()
}
