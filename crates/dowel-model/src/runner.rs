//! 実行ラッパ（docs/30-devexp.md 1節）。
//!
//! クロス実行では成果物をそのまま起動できない。ターゲットトリプルごとに
//! 「何で包んで起動するか」を宣言し、`dowel test` がそれを通す。
//!
//! ```toml
//! [runner.riscv64gc-unknown-linux-gnu]
//! command = "qemu-riscv64"
//! args    = ["-L", "/usr/riscv64-linux-gnu"]
//! ```
//!
//! ## なぜ「無いこと」を診断にするか
//!
//! ホストと違うトリプルを指定していてランナーが宣言されていない場合、
//! 成果物をそのまま起動すると `Exec format error` になる。これはテストの
//! 失敗として報告され、利用者は自分のコードを疑う。原因は構成にあるため、
//! 起動する前に構成の誤りとして出す。

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
