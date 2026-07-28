//! `Session` が読み込みに使うクエリ。
//!
//! ファイルの中身を入力とし、構文解析と評価をその上の導出クエリとして置く。
//! [`Session::reload`](crate::Session::reload) は全ファイルを読み直すが、
//! 中身が変わらなかったファイルは字句解析からやり直さない。
//!
//! ## 粒度を「ファイル単位」にしている理由
//!
//! docs/20-architecture.md 3節が求める early cutoff は、この粒度では
//! 原理的に効かない。値（`Document`）はスパンを含み、スパンはファイル内の
//! バイト位置である。空白1つの挿入でも全てのスパンがずれるため、
//! 「中身が変わったのに評価結果は同じ」という状況が起きない。
//!
//! したがってここでの指紋は入力の指紋から導く。これは
//! 「指紋が同じなら値も同じ」を満たす（同じ本文からは同じ木が出る）。
//! 逆向き（値が同じなら指紋も同じ）は満たさないが、その向きの取りこぼしは
//! 再計算が増えるだけで誤りにはならない。
//!
//! cutoff が意味を持つのはターゲット単位の派生（`interface` の併合結果など）を
//! クエリにしてからであり、その段でスパンを含まない要約に指紋を付ける。
//! 現時点で `Document` の内容指紋を手で書いても、効かない経路に複雑さを足すだけになる。

use dowel_query::{fingerprint_str, Cancelled, Db, Durability, Fingerprint};
use dowel_support::{log_trace, Diagnostic, FileId};
use dowel_syntax::Parsed;
use std::sync::Arc;

/// クエリの鍵。
///
/// `FileId` を鍵にできるのは、[`SourceMap::load`](dowel_support::SourceMap::load)
/// が同じパスに同じ識別子を返すためである。読み直しで識別子が変わるなら、
/// メモに残った値の来歴が別のファイルを指すことになる。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Key {
    /// 入力: ファイルの本文
    Text(FileId),
    /// 導出: 構文解析の結果（木と診断）
    Parsed(FileId),
    /// 導出: 評価結果（文書と診断）
    Evaluated(FileId),
}

/// 評価クエリの値。診断を値に含めるのは、メモが再利用されたときに
/// 計算手続きが走らないためである。副作用として外へ押し出していると、
/// 2回目の読み込みで診断が消える。
pub struct Evaluated {
    pub doc: dowel_eval::Document,
    pub diagnostics: Vec<Diagnostic>,
}

/// 本文を入力として登録する。指紋が同じなら版は進まない。
pub fn set_text(db: &Db<Key>, file: FileId, text: &str) {
    // マニフェストは編集のたびに変わる側であり、耐久度は最も低い。
    // ツールチェーンの事実（Phase 2 のプローブ）が入る際に High を使う。
    db.set_input(Key::Text(file), text.to_string(), fingerprint_str(text), Durability::Low);
}

/// 構文解析。
pub fn parsed(db: &Db<Key>, file: FileId) -> Result<Arc<Parsed>, Cancelled> {
    db.query(Key::Parsed(file), move |db| {
        let src =
            db.input::<String>(Key::Text(file))?.expect("the text input is set before parsing");
        log_trace!("parsing file {} ({} bytes)", file.0, src.len());
        let parsed = dowel_syntax::parse(&src, file);
        log_trace!("  {} parse diagnostics", parsed.diagnostics.len());
        Ok((parsed, fingerprint_of_source(&src)))
    })
}

/// 評価。`strict` は `dowel.toml` にのみ課す追加検証であり、
/// ファイルの種別で決まって変わらない。同じ `file` に別の `strict` で
/// 問い合わせてはならない（メモが黙って食い違う）。
pub fn evaluated(db: &Db<Key>, file: FileId, strict: bool) -> Result<Arc<Evaluated>, Cancelled> {
    db.query(Key::Evaluated(file), move |db| {
        let src = db.input::<String>(Key::Text(file))?.expect("the text input is set before eval");
        let tree = parsed(db, file)?;
        log_trace!("evaluating file {} (strict={strict})", file.0);

        let mut diagnostics = tree.diagnostics.clone();
        if strict {
            diagnostics.extend(dowel_eval::strict::check(&tree.root, file));
        }
        let (doc, diags) = dowel_eval::eval(&tree.root, &src, file);
        diagnostics.extend(diags);
        log_trace!("  {} tables, {} diagnostics", doc.tables.len(), diagnostics.len());
        for table in &doc.tables {
            log_trace!("  table [{}] with {} entries", table.path.join("."), table.entries.len());
            for e in &table.entries {
                log_trace!("    {} = {}", e.key.join("."), e.value.display());
            }
        }
        Ok((Evaluated { doc, diagnostics }, fingerprint_of_source(&src)))
    })
}

/// 導出クエリの指紋。本モジュールの冒頭で述べた理由により、
/// 本文の指紋をそのまま使う。
fn fingerprint_of_source(src: &str) -> Fingerprint {
    fingerprint_str(src)
}
