//! ターゲットとその宣言されたプロパティ。
//!
//! ここに格納する値は具体化前である。`match` と後置 `when` は残っている。
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
    /// `[<kind>.<name>.artifacts]`。成果物から派生させる変換。宣言順
    pub artifacts: Vec<ArtifactDecl>,
}

/// 成果物から別の成果物を作る変換の宣言（issue #60）。
///
/// プロパティの写像に載せないのは、これが**伝播しない別の種類の宣言**で
/// あるためである。`public` / `private` の区別は届く範囲の話であり、
/// 変換はどちらでもない——自分の成果物から自分の成果物を作る。
#[derive(Clone, Debug)]
pub struct ArtifactDecl {
    /// 出力の拡張子。`bin = { ... }` なら `bin`
    pub suffix: String,
    /// 使う道具の名前（`dowel_eval::config::TOOLS` のもの）。
    ///
    /// 構成で分岐させない。トリプルごとに別の実体を使うのは
    /// `[toolchain.<triple>]` の仕事であり、宣言の側は「どの道具か」だけを
    /// 述べる
    pub tool: String,
    /// 道具に渡す引数。入力と出力はこの後ろに実装が付ける（ADR-0008）。
    /// 具体化前の値であり、`when` / `match` を含みうる
    pub args: Option<Value>,
    /// 項目の位置
    pub site: Site,
    /// `tool = "..."` の位置。実在しない道具を指す診断が参照する
    pub tool_site: Site,
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
