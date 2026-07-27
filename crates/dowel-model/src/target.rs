//! ターゲットとその宣言されたプロパティ。
//!
//! ここに格納する値は**具体化前**である。`match` と後置 `when` は残っている。
//! 構成を与えるのはアクション生成の段階であり、この分離が
//! 「`--release` の切り替えでマニフェスト評価をやり直さない」の実体である。

use dowel_eval::schema::{Block, TableKind};
use dowel_eval::{Site, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PackageId(pub usize);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TargetId(pub usize);

/// プロパティ名から宣言された値への写像。
pub type PropMap = BTreeMap<String, Value>;

#[derive(Clone, Debug)]
pub struct Target {
    pub id: TargetId,
    pub package: PackageId,
    pub kind: TableKind,
    pub name: String,
    /// `[lib.foo]` の見出しの位置
    pub site: Site,
    /// 伝播しないプロパティ（`sources` など）
    pub root: PropMap,
    /// 依存側へ伝播するプロパティ
    pub public: PropMap,
    /// 自身のコンパイルにのみ効くプロパティ
    pub private: PropMap,
}

impl Target {
    pub fn props(&self, block: Block) -> &PropMap {
        match block {
            Block::Root => &self.root,
            Block::Public => &self.public,
            Block::Private => &self.private,
        }
    }

    pub fn props_mut(&mut self, block: Block) -> &mut PropMap {
        match block {
            Block::Root => &mut self.root,
            Block::Public => &mut self.public,
            Block::Private => &mut self.private,
        }
    }
}

/// `<パッケージ名>:<ターゲット名>`。表示と `dowel why` の指定に使う。
pub fn label(package_name: &str, target_name: &str) -> String {
    format!("{package_name}:{target_name}")
}
