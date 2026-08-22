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
//! 併合はターゲットの宣言と依存の解決結果から決まる。宣言は評価結果からの
//! **導出**である（[`build_decls`]、[`declared`]）——同じ本文からは同じ宣言が
//! 出るので、触っていないファイルでは組み上げ自体が走らない。
//!
//! 依存の解決も導出である（[`deps`]）。入力に残るのは**名札の表**だけで
//! ある（[`set_name_table`]）——`dep("...")` の解決には「どのディレクトリが
//! どのパッケージか」が要り、それを決めるのは読み込みそのものである。表は
//! 名前しか持たないので、木の形が変わらない限り版は進まない。
//!
//! 鍵をファイル単位（[`Key::BuildDecls`]）とターゲット単位
//! （[`Key::Declared`]）に分けているのは cutoff の粒度のためである。同じ
//! ファイルの別のターゲットを編集すると前者の指紋は変わるが、後者は変わらない
//! ——併合はそこで止まる。
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

use crate::target::{PropMap, TargetDecl};
use dowel_eval::schema::{self, Block};
use dowel_eval::{Config, Data, Value};
use dowel_query::{fingerprint_str, Cancelled, Db, Durability, Fingerprint};
use dowel_support::{log_trace, Diagnostic, FileId};
use dowel_syntax::Parsed;
use std::rc::Rc;
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
    /// 導出: 1つの `dowel.build` が宣言したターゲット一式
    BuildDecls(FileId),
    /// 入力: ラベルの宣言がどこから来るか（[`Source`]）。
    ///
    /// 対応そのものは読み込みが決める（どのディレクトリがどのパッケージか）。
    /// ファイル由来なら値は `FileId` 1つなので、ターゲットがファイルを移らない
    /// 限り版は進まない。
    TargetSource(String),
    /// 導出: ターゲットが宣言したプロパティ。鍵はラベル（`pkg:name`）
    ///
    /// 添字（`TargetId`）を鍵にしない。読み込み順で振られるため、
    /// マニフェストの形が変わるとメモが別のターゲットを指す。
    ///
    /// ファイル単位（[`Key::BuildDecls`]）から1つ取り出すだけの導出だが、
    /// 鍵を分けることで cutoff の粒度がターゲット単位に保たれる——同じ
    /// ファイルの別のターゲットを編集しても、こちらの指紋は変わらない。
    Declared(String),
    /// 入力: 名前解決に要る、全パッケージ分の名札の表
    ///
    /// これだけは1ファイルからの導出にならない。`dep("...")` の解決には
    /// **どのディレクトリがどのパッケージか**が要り、それを決めるのは
    /// 読み込みそのものである。値は名前だけなので、木の形が変わらない限り
    /// 版は進まない。
    NameTable,
    /// 導出: 解決済みの依存。ラベルと宣言ブロックの対
    Deps(String),
    /// 導出: 依存側へ供給するプロパティ
    Interface(String),
    /// 導出: 自身のコンパイルに効くプロパティ
    CompileEnv(String),
}

/// ターゲットが宣言したプロパティ。具体化前。
///
/// 宣言そのものは [`BuildDecls`] が持ち、ここはその1つを指すだけである。
pub struct Declared(pub Arc<TargetDecl>);

impl std::ops::Deref for Declared {
    type Target = TargetDecl;

    fn deref(&self) -> &TargetDecl {
        &self.0
    }
}

/// 1つの `dowel.build` が宣言したもの一式。
///
/// 読み込みそのものを導出にするための単位である（Phase 1 の宿題）。
/// 以前は `Session` が読み込みの度に組み上げ、入力として渡していた——
/// 触っていないファイルでも、値の写しと要約の計算が毎回走っていた。
pub struct BuildDecls {
    pub targets: Vec<Arc<TargetDecl>>,
    pub runners: std::collections::BTreeMap<String, crate::runner::Runner>,
    pub diagnostics: Vec<Diagnostic>,
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
pub fn parsed(db: &Db<Key>, file: FileId, max_nesting: usize) -> Result<Arc<Parsed>, Cancelled> {
    db.query(Key::Parsed(file), move |db| {
        let src =
            db.input::<String>(Key::Text(file))?.expect("the text input is set before parsing");
        log_trace!("parsing file {} ({} bytes)", file.0, src.len());
        let parsed = dowel_syntax::parse_with_max_nesting(&src, file, max_nesting);
        log_trace!("  {} parse diagnostics", parsed.diagnostics.len());
        Ok((parsed, fingerprint_of_source(&src, max_nesting)))
    })
}

/// プロセスを跨いだ評価結果の供給元（[ADR-0012](../../../docs/adr/0012-store-contents.md)）。
///
/// クエリからストアの形式を見えなくするために挟む。実装は
/// [`crate::persist::Cache`]。
pub trait Evaluations {
    /// 本文の指紋が `fingerprint` であるファイルの評価結果。
    ///
    /// 診断を持つファイルは格納されないため、返る文書に対応する診断は無い。
    fn get(&self, file: FileId, fingerprint: u64) -> Option<dowel_eval::Document>;

    /// 診断を出さずに計算できた結果を渡す。格納するかは実装が決める。
    fn put(&self, file: FileId, fingerprint: u64, doc: &dowel_eval::Document);

    /// 診断があったため渡さなかったことを伝える。数え上げのためだけに持つ。
    fn skipped(&self, file: FileId);
}

/// 評価。`strict` は `dowel.toml` にのみ課す追加検証であり、
/// ファイルの種別で決まって変わらない。同じ `file` に別の `strict` で
/// 問い合わせてはならない（メモが黙って食い違う）。
///
/// `store` を与えると、本文が前回と同じファイルは解析も評価もせずに復元する。
/// `strict` は診断を足すだけで文書を変えないため、復元の可否に関わらない。
pub fn evaluated(
    db: &Db<Key>,
    file: FileId,
    strict: bool,
    max_nesting: usize,
    store: Option<Rc<dyn Evaluations>>,
) -> Result<Arc<Evaluated>, Cancelled> {
    db.query(Key::Evaluated(file), move |db| {
        let src = db.input::<String>(Key::Text(file))?.expect("the text input is set before eval");
        let fp = fingerprint_of_source(&src, max_nesting);
        // 復元は解析の前に試す。復元できた場合、`Parsed` のメモは作られない。
        if let Some(store) = &store {
            if let Some(doc) = store.get(file, fp) {
                return Ok((Evaluated { doc, diagnostics: Vec::new() }, fp));
            }
        }
        let tree = parsed(db, file, max_nesting)?;
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
        if let Some(store) = &store {
            if diagnostics.is_empty() {
                store.put(file, fp, &doc);
            } else {
                store.skipped(file);
            }
        }
        Ok((Evaluated { doc, diagnostics }, fp))
    })
}

/// 導出クエリの指紋。本モジュールの冒頭で述べた理由により、
/// 本文の指紋から導く。
///
/// 入れ子の上限も混ぜる。同じ本文でも上限が違えば診断が違いうるため、
/// 混ぜないと上限を跨いだ再実行でストアが古い結果を返す（深いマニフェストを
/// `--max-nesting` を上げて評価・格納した後、既定で実行しても診断が出ない）。
/// 既定の上限では本文の指紋をそのまま使い、既存のストアと互換に保つ。
fn fingerprint_of_source(src: &str, max_nesting: usize) -> Fingerprint {
    if max_nesting == dowel_syntax::MAX_NESTING {
        return fingerprint_str(src);
    }
    dowel_query::fingerprint_of(&(fingerprint_str(src), max_nesting as u64))
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

/// 読み込みに要る設定。導出クエリが互いを呼ぶときに持ち回る。
///
/// 入力（`Db` の鍵）にはできない。`store` は `Rc<dyn Evaluations>` であり、
/// 入力に求められる `Send + Sync` を満たさない——プロセスを跨いだ供給元は
/// スレッドを跨がない。
#[derive(Clone)]
pub struct Ctx {
    pub max_nesting: usize,
    pub store: Option<Rc<dyn Evaluations>>,
}

/// 1つの `dowel.build` が宣言したターゲット一式。
///
/// 評価結果からの導出である。同じ本文からは同じ宣言が出るので、触っていない
/// ファイルではここも走らない。診断を値に含めるのは [`Evaluated`] と同じ
/// 理由による——メモが再利用されると計算手続きが走らない。
pub fn build_decls(db: &Db<Key>, file: FileId, ctx: &Ctx) -> Result<Arc<BuildDecls>, Cancelled> {
    let ctx = ctx.clone();
    db.query(Key::BuildDecls(file), move |db| {
        let doc = evaluated(db, file, false, ctx.max_nesting, ctx.store.clone())?;
        let decls = crate::session::declarations_of(&doc.doc);
        let fp = build_decls_fingerprint(&decls);
        Ok((decls, fp))
    })
}

/// ターゲットの宣言の出どころ。
///
/// ほとんどのターゲットは `dowel.build` から来る。pkg-config が答えた面だけを
/// 持つ外部のターゲットには読む文書が無いので、宣言そのものを渡す
/// （[ADR-0015](../../../docs/adr/0015-version-deps-pkgconfig.md)）。
#[derive(Clone)]
pub enum Source {
    File(FileId),
    External(Arc<TargetDecl>),
}

/// ラベルの宣言の出どころを登録する。
pub fn set_target_source(db: &Db<Key>, label: &str, source: Source) {
    let fp = match &source {
        Source::File(f) => dowel_query::fingerprint_of(&f.0),
        Source::External(d) => declared_fingerprint(d),
    };
    db.set_input(Key::TargetSource(label.to_string()), source, fp, Durability::Low);
}

/// ターゲット1つの宣言。指紋はスパンを含まない要約から導く。
///
/// ファイル単位の宣言から取り出すだけだが、鍵を分けることで cutoff の粒度が
/// ターゲット単位に保たれる。同じファイルの別のターゲットを編集しても、
/// この指紋は変わらないので併合まで届かない。
pub fn declared(db: &Db<Key>, label: &str, ctx: &Ctx) -> Result<Arc<Declared>, Cancelled> {
    let owned = label.to_string();
    let ctx = ctx.clone();
    db.query(Key::Declared(owned.clone()), move |db| {
        let source = db
            .input::<Source>(Key::TargetSource(owned.clone()))?
            .expect("the target's source is set before merging");
        let name = owned.split_once(':').map(|(_, n)| n).unwrap_or(&owned);
        let decl = match source.as_ref() {
            Source::External(d) => d.clone(),
            Source::File(file) => {
                let decls = build_decls(db, *file, &ctx)?;
                decls
                    .targets
                    .iter()
                    .find(|t| t.name == name)
                    .cloned()
                    // 宣言が消えた場合。読み込みが登録し直すまでの間だけ在りうる。
                    .unwrap_or_else(|| {
                        Arc::new(TargetDecl::bare(
                            dowel_eval::schema::TableKind::Lib,
                            name.to_string(),
                            dowel_eval::Site::new(*file, dowel_support::Span::EMPTY),
                        ))
                    })
            }
        };
        let fp = declared_fingerprint(&decl);
        Ok((Declared(decl), fp))
    })
}

/// 宣言1つ分の指紋。スパンを含まない要約から導く（ADR-0011）。
fn declared_fingerprint(decl: &TargetDecl) -> Fingerprint {
    dowel_query::fingerprint_of(&(
        dowel_eval::props_digest(decl.public.iter().map(|(k, v)| (k.as_str(), v))),
        dowel_eval::props_digest(decl.private.iter().map(|(k, v)| (k.as_str(), v))),
    ))
}

/// ファイル1つ分の指紋。
///
/// 根のプロパティと事例・変換・検査の宣言も混ぜる。併合に効くのは
/// `public` / `private` だけだが、この値を読むのは併合だけではない——
/// `sources` の編集で読み込みが組み直されなければ、計画が古い宣言を見る。
fn build_decls_fingerprint(decls: &BuildDecls) -> Fingerprint {
    let mut parts: Vec<u64> = Vec::new();
    for t in &decls.targets {
        parts.push(fingerprint_str(t.name.as_str()));
        parts.push(dowel_eval::props_digest(t.root.iter().map(|(k, v)| (k.as_str(), v))));
        parts.push(declared_fingerprint(t));
        parts.push(dowel_query::fingerprint_of(&(
            t.artifacts.len() as u64,
            t.inspections.len() as u64,
            t.cases.len() as u64,
            t.harness.is_some(),
            t.generated.len() as u64,
        )));
        for g in &t.generated {
            parts.push(fingerprint_str(&g.name));
            parts.push(fingerprint_str(&g.command));
            parts.push(dowel_query::fingerprint_of(&g.public));
            for v in [&g.args, &g.inputs, &g.outputs] {
                parts.push(v.as_ref().map(dowel_eval::value_digest).unwrap_or(0));
            }
        }
        for c in &t.cases {
            parts.push(fingerprint_str(&c.name));
            parts.push(dowel_eval::value_digest(&c.value));
        }
        for a in t.artifacts.iter().chain(&t.inspections) {
            parts.push(fingerprint_str(&a.suffix));
            parts.push(fingerprint_str(&a.tool));
            parts.push(a.args.as_ref().map(dowel_eval::value_digest).unwrap_or(0));
        }
        if let Some(h) = &t.harness {
            for (k, v) in &h.fields {
                parts.push(fingerprint_str(k));
                parts.push(dowel_eval::value_digest(v));
            }
        }
    }
    for (triple, r) in &decls.runners {
        parts.push(fingerprint_str(triple));
        parts.push(fingerprint_str(&format!("{r:?}")));
    }
    // 診断はスパンを含む。位置が動けば cutoff は起きない——誤りのある
    // 構成では組み直しを選び、古い位置を報告しない。
    parts.push(diagnostics_fingerprint(&decls.diagnostics));
    dowel_query::fingerprint_of(&parts)
}

/// 解決済みの依存を入力として登録する。
pub fn set_name_table(db: &Db<Key>, table: NameTable) {
    let fp = fingerprint_str(&table.canonical());
    db.set_input(Key::NameTable, table, fp, Durability::Low);
}

/// 名前解決の結果。`dowel.toml` の依存1つ分。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DepResolution {
    /// 解決できた依存先のパッケージ名
    Package(String),
    /// 任意の依存で、対応する機能が有効でない。読み込んでいないため解決できない
    Inactive,
    /// 供給形態が未実装。`dowel.toml` の読み取りで診断済みである
    AlreadyReported,
}

/// 名前解決に要る、全パッケージ分の名札の表。
///
/// 値は持たない——名前と種別だけである。宣言そのもの（[`Declared`]）と
/// 分けているのは、こちらが**木の形**で決まるためである。1ファイルを
/// 編集しても、ターゲットの名前が変わらない限りこの表は動かない。
#[derive(Default)]
pub struct NameTable {
    /// パッケージ名 → そのパッケージのターゲット（名前と種別、宣言順）
    pub targets: std::collections::BTreeMap<String, Vec<(String, schema::TableKind)>>,
    /// パッケージ名 → 宣言された依存の名前と解決結果（宣言順）
    pub deps: std::collections::BTreeMap<String, Vec<(String, DepResolution)>>,
}

impl NameTable {
    /// 指紋を取るための正規形。中身が同じなら同じ文字列になる。
    fn canonical(&self) -> String {
        let mut out = String::new();
        for (pkg, targets) in &self.targets {
            out.push_str(pkg);
            for (name, kind) in targets {
                out.push('\u{1}');
                out.push_str(name);
                out.push('\u{2}');
                out.push_str(kind.name());
            }
            out.push('\n');
        }
        for (pkg, deps) in &self.deps {
            out.push_str(pkg);
            for (name, r) in deps {
                out.push('\u{1}');
                out.push_str(name);
                out.push('\u{2}');
                match r {
                    DepResolution::Package(p) => out.push_str(p),
                    DepResolution::Inactive => out.push_str("<inactive>"),
                    DepResolution::AlreadyReported => out.push_str("<reported>"),
                }
            }
            out.push('\n');
        }
        out
    }

    /// このパッケージのターゲットのうち、この名前のもの。
    fn target_in(&self, pkg: &str, name: &str) -> bool {
        self.targets.get(pkg).is_some_and(|ts| ts.iter().any(|(n, _)| n == name))
    }

    fn target_names(&self, pkg: &str) -> Vec<&str> {
        self.targets
            .get(pkg)
            .map(|ts| ts.iter().map(|(n, _)| n.as_str()).collect())
            .unwrap_or_default()
    }

    fn dep_names(&self, pkg: &str) -> Vec<&str> {
        self.deps
            .get(pkg)
            .map(|ds| ds.iter().map(|(n, _)| n.as_str()).collect())
            .unwrap_or_default()
    }

    fn resolve_dep(&self, pkg: &str, name: &str) -> Option<&DepResolution> {
        self.deps.get(pkg)?.iter().find(|(n, _)| n == name).map(|(_, r)| r)
    }

    /// 依存先パッケージが供給するライブラリのラベル。
    fn libs_of(&self, pkg: &str) -> Vec<String> {
        self.targets
            .get(pkg)
            .map(|ts| {
                ts.iter()
                    .filter(|(_, k)| *k == schema::TableKind::Lib)
                    .map(|(n, _)| crate::target::label(pkg, n))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 解決済みの依存。
pub struct Deps {
    /// 依存先のラベル、宣言したブロック、書かれた位置
    pub edges: Vec<(String, Block, dowel_eval::Site)>,
    pub diagnostics: Vec<Diagnostic>,
}

/// 解決済みの依存（[`Key::Deps`]）。
///
/// 辺は具体化後に決まる。`deps = [dep("zlib") when feature.zlib]` は機能
/// フラグによって現れたり消えたりするので、構成なしには定まらない。
///
/// 指紋には**ラベルとブロックと診断だけ**を混ぜる。位置は混ぜない——
/// コメント1行の挿入で全ての位置が動くが、併合が読むのは「誰に繋がるか」
/// だけである（ADR-0011 と同じ判断）。位置は値には持つ。cutoff でも値は
/// 差し替わるため、閉路の診断が古い位置を指すことはない。
pub fn deps(db: &Db<Key>, label: &str, ctx: &Ctx) -> Result<Arc<Deps>, Cancelled> {
    let owned = label.to_string();
    let ctx = ctx.clone();
    db.query(Key::Deps(owned.clone()), move |db| {
        let declared = expect_declared(db, &owned, &ctx)?;
        let cfg = expect_config(db)?.for_package(package_of(&owned));
        let names =
            db.input::<NameTable>(Key::NameTable)?.expect("the name table is set before resolving");
        let pkg = package_of(&owned);
        let mut edges = Vec::new();
        let mut diagnostics = Vec::new();
        for block in [Block::Public, Block::Private] {
            let Some(value) = declared.props(block).get("deps") else { continue };
            let Some(value) = dowel_eval::specialize(value, &cfg) else { continue };
            for item in dep_items(&value) {
                let site = item.prov.nearest_site().unwrap_or(declared.site);
                match &item.data {
                    Data::Target(name) if names.target_in(pkg, name) => {
                        edges.push((crate::target::label(pkg, name), block, site));
                    }
                    Data::Target(name) => diagnostics.push(crate::graph::unknown_target(
                        name,
                        &names.target_names(pkg),
                        &item,
                    )),
                    Data::Dep(name) => match names.resolve_dep(pkg, name) {
                        None => diagnostics.push(crate::graph::undeclared_dep(
                            name,
                            &names.dep_names(pkg),
                            &item,
                        )),
                        Some(DepResolution::Inactive) => {
                            diagnostics.push(crate::graph::inactive_dep(name, &item))
                        }
                        // 供給形態が未実装なものは `dowel.toml` の読み取りで
                        // 既に診断済み。同じことを2度言わない。
                        Some(DepResolution::AlreadyReported) => {}
                        Some(DepResolution::Package(to)) => {
                            let libs = names.libs_of(to);
                            if libs.is_empty() {
                                diagnostics.push(crate::graph::empty_dep(name, &item));
                            }
                            for l in libs {
                                edges.push((l, block, site));
                            }
                        }
                    },
                    Data::Error => {}
                    _ => diagnostics.push(crate::graph::invalid_dep(&item)),
                }
            }
        }
        let key: Vec<(&str, u8)> = edges.iter().map(|(l, b, _)| (l.as_str(), *b as u8)).collect();
        let fp = dowel_query::fingerprint_of(&(
            dowel_query::fingerprint_of(&key),
            diagnostics_fingerprint(&diagnostics),
        ));
        Ok((Deps { edges, diagnostics }, fp))
    })
}

/// `deps` の値を1件ずつに開く。
fn dep_items(value: &Value) -> Vec<Value> {
    match &value.data {
        Data::List(items) => items.clone(),
        Data::Error => Vec::new(),
        _ => vec![value.clone()],
    }
}

/// ラベル `<パッケージ>:<ターゲット>` のパッケージ名。
fn package_of(label: &str) -> &str {
    label.split_once(':').map(|(p, _)| p).unwrap_or("")
}

/// 依存側へ供給するプロパティ。
///
/// `interface(T)` = T の `public` ＋ T の `public.deps` の `interface`
/// （[`crate::interface`] の定義と同じ）。
pub fn interface(db: &Db<Key>, label: &str, ctx: &Ctx) -> Result<Arc<Merged>, Cancelled> {
    let owned = label.to_string();
    let ctx = ctx.clone();
    db.query(Key::Interface(owned.clone()), move |db| {
        let declared = expect_declared(db, &owned, &ctx)?;
        // 具体化はそのターゲットのパッケージで行う。`feature.<名前>` は
        // 宣言したパッケージで有効かを問うものである（ADR-0017）。
        let cfg = expect_config(db)?.for_package(package_of(&owned));
        let deps = expect_deps(db, &owned, &ctx)?;
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
            for (dep, ..) in deps.edges.iter().filter(|(_, b, _)| *b == Block::Public) {
                if let Some(v) = interface(db, dep, &ctx)?.props.get(def.name) {
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
pub fn compile_env(db: &Db<Key>, label: &str, ctx: &Ctx) -> Result<Arc<Merged>, Cancelled> {
    let owned = label.to_string();
    let ctx = ctx.clone();
    db.query(Key::CompileEnv(owned.clone()), move |db| {
        let declared = expect_declared(db, &owned, &ctx)?;
        // 具体化はそのターゲットのパッケージで行う。`feature.<名前>` は
        // 宣言したパッケージで有効かを問うものである（ADR-0017）。
        let cfg = expect_config(db)?.for_package(package_of(&owned));
        let deps = expect_deps(db, &owned, &ctx)?;
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
            for (dep, ..) in deps.edges.iter() {
                if let Some(v) = interface(db, dep, &ctx)?.props.get(def.name) {
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

fn expect_declared(db: &Db<Key>, label: &str, ctx: &Ctx) -> Result<Arc<Declared>, Cancelled> {
    declared(db, label, ctx)
}

fn expect_config(db: &Db<Key>) -> Result<Arc<Config>, Cancelled> {
    Ok(db.input::<Config>(Key::Config)?.expect("the configuration is set before merging"))
}

fn expect_deps(db: &Db<Key>, label: &str, ctx: &Ctx) -> Result<Arc<Deps>, Cancelled> {
    deps(db, label, ctx)
}

/// 併合結果の指紋。
///
/// プロパティはスパンを含まない要約から、診断はスパンを含む形から導く。
/// 誤りのある構成で位置が動いた場合は cutoff させず、再計算して新しい位置を出す。
fn merged_fingerprint(props: &PropMap, diagnostics: &[Diagnostic]) -> Fingerprint {
    let summary = dowel_eval::props_digest(props.iter().map(|(k, v)| (k.as_str(), v)));
    dowel_query::fingerprint_of(&(summary, diagnostics_fingerprint(diagnostics)))
}

/// 診断の並びの指紋。スパンを含むので、位置が動けば変わる。
fn diagnostics_fingerprint(diagnostics: &[Diagnostic]) -> Fingerprint {
    let mut of_diags = Vec::new();
    for d in diagnostics {
        let labels: Vec<(u64, u32, u32)> =
            d.labels.iter().map(|l| (l.file.0, l.span.start, l.span.end)).collect();
        of_diags.push((d.code, d.message.clone(), labels, d.notes.clone()));
    }
    dowel_query::fingerprint_of(&of_diags)
}

fn names(props: &PropMap) -> String {
    props.keys().cloned().collect::<Vec<_>>().join(", ")
}
