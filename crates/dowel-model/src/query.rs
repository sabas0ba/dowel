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
//! cutoff が効くのはターゲット単位の派生（[`interface`] と [`compile_env`]）である。
//! これらの指紋は [`dowel_eval::digest`] が求める要約から導く。要約はスパンを
//! 含まないため、コメントや空白の編集では変わらない。
//!
//! ## 派生クエリの入力
//!
//! 併合はターゲットの宣言と依存の解決結果から決まる。どちらも `Session` が
//! 組み上げるため、クエリからは入力として受け取る（[`set_declared`]、
//! [`set_deps`]）。読み込みと名前解決そのものをクエリにするのは別の増分である。
//!
//! ## 来歴の扱い
//!
//! 派生の値は来歴を持つが、そのスパンは cutoff の後で古くなりうる。
//! 下流（アクション生成）が来歴から読むのは宣言位置のファイルだけであり、
//! これは要約に含めているため一致する。スパンを読む経路（`dowel why`）は
//! メモを経由せず、その場で併合をやり直す。
//!
//! 診断は値に含め、指紋にも含める。スパンを含むため、位置が動けば cutoff は
//! 起きない。誤りのある構成では再計算を選び、古い位置を報告しない。

use crate::target::PropMap;
use dowel_eval::schema::{self, Block};
use dowel_eval::{Config, Value};
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
    /// 入力: 構成。`--release` や `--target` の切り替えで変わる
    Config,
    /// 入力: ターゲットが宣言したプロパティ。鍵はラベル（`pkg:name`）
    ///
    /// 添字（`TargetId`）を鍵にしない。読み込み順で振られるため、
    /// マニフェストの形が変わるとメモが別のターゲットを指す。
    Declared(String),
    /// 入力: 解決済みの依存。ラベルと宣言ブロックの対
    Deps(String),
    /// 導出: 依存側へ供給するプロパティ
    Interface(String),
    /// 導出: 自身のコンパイルに効くプロパティ
    CompileEnv(String),
}

/// ターゲットが宣言したプロパティ。具体化前。
pub struct Declared {
    pub public: PropMap,
    pub private: PropMap,
}

/// 併合の結果。診断を値に含めるのは [`Evaluated`] と同じ理由による。
pub struct Merged {
    pub props: PropMap,
    pub diagnostics: Vec<Diagnostic>,
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

// ---------------------------------------------------------------------------
// ターゲット単位の派生
// ---------------------------------------------------------------------------

/// 構成を入力として登録する。
///
/// 耐久度は Medium。編集のたびには変わらないが、`--release` の切り替えでは変わる。
pub fn set_config(db: &Db<Key>, cfg: &Config) {
    db.set_input(
        Key::Config,
        cfg.clone(),
        dowel_query::fingerprint_of(&cfg.id()),
        Durability::Medium,
    );
}

/// ターゲットの宣言を入力として登録する。指紋はスパンを含まない要約から導く。
pub fn set_declared(db: &Db<Key>, label: &str, public: PropMap, private: PropMap) {
    let fp = dowel_query::fingerprint_of(&(
        dowel_eval::props_digest(public.iter().map(|(k, v)| (k.as_str(), v))),
        dowel_eval::props_digest(private.iter().map(|(k, v)| (k.as_str(), v))),
    ));
    db.set_input(
        Key::Declared(label.to_string()),
        Declared { public, private },
        fp,
        Durability::Low,
    );
}

/// 解決済みの依存を入力として登録する。
pub fn set_deps(db: &Db<Key>, label: &str, deps: Vec<(String, Block)>) {
    let key: Vec<(String, u8)> = deps.iter().map(|(l, b)| (l.clone(), *b as u8)).collect();
    let fp = dowel_query::fingerprint_of(&key);
    db.set_input(Key::Deps(label.to_string()), deps, fp, Durability::Low);
}

/// 依存側へ供給するプロパティ。
///
/// `interface(T)` = T の `public` ＋ T の `public.deps` の `interface`
/// （[`crate::interface`] の定義と同じ）。
pub fn interface(db: &Db<Key>, label: &str) -> Result<Arc<Merged>, Cancelled> {
    let owned = label.to_string();
    db.query(Key::Interface(owned.clone()), move |db| {
        let declared = expect_declared(db, &owned)?;
        let cfg = expect_config(db)?;
        let deps = expect_deps(db, &owned)?;
        let mut diagnostics = Vec::new();
        let mut props = PropMap::new();
        for def in schema::block_props() {
            let mut reached: Vec<Value> = Vec::new();
            if let Some(v) = declared.public.get(def.name) {
                if let Some(v) = dowel_eval::specialize(v, &cfg) {
                    reached.push(v);
                }
            }
            // 伝播するのは `public` で宣言された依存だけである。
            for (dep, _) in deps.iter().filter(|(_, b)| *b == Block::Public) {
                if let Some(v) = interface(db, dep)?.props.get(def.name) {
                    reached.push(crate::interface::tag_propagated(v, dep, def.name));
                }
            }
            if reached.is_empty() {
                continue;
            }
            let merged = schema::merge_values(&def, &reached, &mut diagnostics);
            // 併合は「どの値が到達したか」を見ないと結果を説明できない。
            // `dowel why` は1つの値を掘るが、こちらは全体を並べて見せる。
            log_trace!(
                "  interface {owned}.{} ({}): {} reached -> {}",
                def.name,
                def.merge.name(),
                reached.len(),
                merged.display()
            );
            props.insert(def.name.to_string(), merged);
        }
        log_trace!("interface({owned}) = {}", names(&props));
        let fp = merged_fingerprint(&props, &diagnostics);
        Ok((Merged { props, diagnostics }, fp))
    })
}

/// 自身のコンパイルに効くプロパティ。
///
/// `compile_env(T)` = T の `public` ＋ T の `private` ＋ 全依存の `interface`。
pub fn compile_env(db: &Db<Key>, label: &str) -> Result<Arc<Merged>, Cancelled> {
    let owned = label.to_string();
    db.query(Key::CompileEnv(owned.clone()), move |db| {
        let declared = expect_declared(db, &owned)?;
        let cfg = expect_config(db)?;
        let deps = expect_deps(db, &owned)?;
        let mut diagnostics = Vec::new();
        let mut props = PropMap::new();
        for def in schema::block_props() {
            let mut reached: Vec<Value> = Vec::new();
            for block in [&declared.public, &declared.private] {
                if let Some(v) = block.get(def.name) {
                    if let Some(v) = dowel_eval::specialize(v, &cfg) {
                        reached.push(v);
                    }
                }
            }
            // 依存は宣言順。`public` と `private` の双方を取り込む。
            for (dep, _) in deps.iter() {
                if let Some(v) = interface(db, dep)?.props.get(def.name) {
                    reached.push(crate::interface::tag_propagated(v, dep, def.name));
                }
            }
            if reached.is_empty() {
                continue;
            }
            let merged = schema::merge_values(&def, &reached, &mut diagnostics);
            log_trace!(
                "  compile_env {owned}.{} ({}): {} reached -> {}",
                def.name,
                def.merge.name(),
                reached.len(),
                merged.display()
            );
            props.insert(def.name.to_string(), merged);
        }
        log_trace!("compile_env({owned}) = {}", names(&props));
        let fp = merged_fingerprint(&props, &diagnostics);
        Ok((Merged { props, diagnostics }, fp))
    })
}

fn expect_declared(db: &Db<Key>, label: &str) -> Result<Arc<Declared>, Cancelled> {
    Ok(db
        .input::<Declared>(Key::Declared(label.to_string()))?
        .expect("the declared properties are set before merging"))
}

fn expect_config(db: &Db<Key>) -> Result<Arc<Config>, Cancelled> {
    Ok(db.input::<Config>(Key::Config)?.expect("the configuration is set before merging"))
}

fn expect_deps(db: &Db<Key>, label: &str) -> Result<Arc<Vec<(String, Block)>>, Cancelled> {
    Ok(db
        .input::<Vec<(String, Block)>>(Key::Deps(label.to_string()))?
        .expect("the resolved dependencies are set before merging"))
}

/// 併合結果の指紋。
///
/// プロパティはスパンを含まない要約から、診断はスパンを含む形から導く。
/// 誤りのある構成で位置が動いた場合は cutoff させず、再計算して新しい位置を出す。
fn merged_fingerprint(props: &PropMap, diagnostics: &[Diagnostic]) -> Fingerprint {
    let summary = dowel_eval::props_digest(props.iter().map(|(k, v)| (k.as_str(), v)));
    let mut of_diags = Vec::new();
    for d in diagnostics {
        let labels: Vec<(u64, u32, u32)> =
            d.labels.iter().map(|l| (l.file.0, l.span.start, l.span.end)).collect();
        of_diags.push((d.code, d.message.clone(), labels, d.notes.clone()));
    }
    dowel_query::fingerprint_of(&(summary, of_diags))
}

fn names(props: &PropMap) -> String {
    props.keys().cloned().collect::<Vec<_>>().join(", ")
}
