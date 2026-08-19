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

/// 1つのファイルが宣言したターゲット。
///
/// **どのセッションに属するかを含まない。** 同じ本文からは同じ宣言が出る
/// ので、これは評価結果からの導出であり、メモに載る
/// （[`crate::query::build_decls`]）。読み込みの度に組み直していたものを、
/// ファイルが変わらない限り組み直さないための分割である。
#[derive(Clone, Debug)]
pub struct TargetDecl {
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
    /// `[<kind>.<name>.inspect]`。成果物について報告する検査。宣言順
    pub inspections: Vec<ArtifactDecl>,
    /// `[test.<name>.cases]`。1本の実行ファイルから登録するテスト。宣言順。
    /// 空なら、そのターゲット自身が1件のテストである（従来の形）
    pub cases: Vec<CaseDecl>,
    /// `[test.<name>.harness]`。実行ファイル自身に事例を列挙させる宣言。
    /// `cases` と同時には書けない——どちらも「事例は何か」に答えるものである
    pub harness: Option<HarnessDecl>,
}

/// セッションの中に置かれたターゲット。宣言そのものは共有される。
///
/// 宣言（[`TargetDecl`]）へは [`std::ops::Deref`] で透過する。`target.kind`
/// や `target.public` は、どちらに在るかを読み手が意識せずに済む。
#[derive(Clone, Debug)]
pub struct Target {
    pub id: TargetId,
    pub package: PackageId,
    /// 宣言。メモから来るので、読み込みごとの写しは Arc 1つ分である
    pub decl: std::sync::Arc<TargetDecl>,
}

impl std::ops::Deref for Target {
    type Target = TargetDecl;

    fn deref(&self) -> &TargetDecl {
        &self.decl
    }
}

/// `[test.<name>.harness]`（ADR-0023）。
///
/// 事例の在り処が実行ファイルの中である場合の宣言。dowel は枠組みを知らず、
/// 「どう尋ねるか」だけをここから読む。
#[derive(Clone, Debug)]
pub struct HarnessDecl {
    /// `dowel_eval::schema::harness_props` の名前 → 値
    pub fields: std::collections::BTreeMap<String, Value>,
    pub site: Site,
}

/// `[test.<name>.cases]` の1項目。
///
/// 事例は**同じ実行ファイルの別の起動**である。翻訳の単位は増えない。
/// 値は具体化前であり、`when` / `match` を含みうる——引数や時間切れを
/// 構成で変えられる。
#[derive(Clone, Debug)]
pub struct CaseDecl {
    /// 事例の名前。ラベルは `<パッケージ>:<ターゲット>/<名前>` になる
    pub name: String,
    /// 事例そのもの。インライン表、あるいはそれを包む `match` / `when`。
    ///
    /// 具体化前の値を丸ごと持つ。事例の**存在**を構成で分けられるように
    /// するためである（issue #92）——実機でしか意味を持たない事例、
    /// エミュレータの下では終わらない事例は、値を変えるのではなく落としたい
    pub value: Value,
    pub site: Site,
}

/// 成果物に対して道具を1つ走らせる宣言（issue #60）。
///
/// `artifacts` の項目（変換）と `inspect` の項目（検査）が同じ形を採る。
/// 違いは出力があるかどうかだけであり、それは置かれたブロックが決める。
///
/// プロパティの写像に載せないのは、これが**伝播しない別の種類の宣言**で
/// あるためである。`public` / `private` の区別は届く範囲の話であり、
/// 変換も検査もどちらでもない——自分の成果物に対して自分が行う。
#[derive(Clone, Debug)]
pub struct ArtifactDecl {
    /// 変換なら出力の拡張子（`bin = { ... }` なら `bin`）、検査なら
    /// 表示に使う名前
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

impl TargetDecl {
    /// 名前とその位置だけを持つ、空の宣言。
    pub fn bare(kind: TableKind, name: String, site: Site) -> TargetDecl {
        TargetDecl {
            kind,
            name,
            site,
            root: PropMap::new(),
            public: PropMap::new(),
            private: PropMap::new(),
            artifacts: Vec::new(),
            inspections: Vec::new(),
            cases: Vec::new(),
            harness: None,
        }
    }

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
