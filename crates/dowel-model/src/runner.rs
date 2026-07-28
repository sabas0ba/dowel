//! 実行ラッパ（docs/30-devexp.md 1節）。
//!
//! クロス実行では成果物をそのまま起動できない。ターゲットトリプルごとに
//! 起動に用いるラッパを宣言し、`dowel test` がそれを経由して起動する。
//!
//! ```toml
//! [runner.riscv64gc-unknown-linux-gnu]
//! command = "qemu-riscv64"
//! args    = ["-L", "/usr/riscv64-linux-gnu"]
//! ```
//!
//! ## ランナーが宣言されていない場合を診断とする理由
//!
//! ホストと異なるトリプルを指定し、かつランナーが宣言されていない場合、
//! 成果物をそのまま起動すると `Exec format error` になり、テストの失敗として
//! 報告される。原因は構成にあってテスト対象のコードにはないため、
//! 起動前に構成の診断として出す。

use dowel_eval::{Site, Value};
use std::collections::BTreeMap;

/// 1つのターゲットトリプルに対する実行ラッパ。
#[derive(Clone, Debug)]
pub struct Runner {
    /// 対象のターゲットトリプル。`[runner.<triple>]` の `<triple>`
    pub triple: String,
    /// `[runner.<triple>]` の見出しの位置
    pub site: Site,
    /// 宣言されたプロパティ。具体化前（`match` と後置 `when` が残っている）
    pub props: BTreeMap<String, Value>,
}

impl Runner {
    pub fn prop(&self, name: &str) -> Option<&Value> {
        self.props.get(name)
    }
}
