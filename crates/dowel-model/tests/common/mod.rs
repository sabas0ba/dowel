//! テスト用の一時パッケージ。
//!
//! リポジトリ外（`/tmp` 等）には作らない（docs/50-development.md 5節）。
//! `target/` 配下は git ignore 済みであり、`cargo clean` で消える。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct Scratch {
    pub root: PathBuf,
}

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = workspace_target().join("test-scratch").join(format!("{name}-{n}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("一時ディレクトリを作れない");
        Scratch { root }
    }

    /// `rel` にファイルを書く。途中のディレクトリは作る。
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
}

fn workspace_target() -> PathBuf {
    // `CARGO_MANIFEST_DIR` は crates/<name>。ワークスペースルートはその2つ上。
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").to_path_buf()
}
