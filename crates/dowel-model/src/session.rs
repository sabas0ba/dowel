//! マニフェストの読み込みとターゲットの構築。
//!
//! `Session` は「1回の CLI 実行が触れた全て」を保持する。増分クエリエンジンと
//! 永続化ストア（docs/20-architecture.md 5節）を差し込む先はここであり、
//! 現時点では素朴に全部を読む実装が入っている。
//! 外から見た形（`load` して `targets` と `graph` を得る）を変えずに
//! 内側を置き換えられるよう、読み込みの入口をこの1箇所に閉じてある。

use crate::package::{self, DepKind, Package};
use crate::target::{label, PackageId, PropMap, Target, TargetId};
use dowel_eval::schema::{self, Block, TableKind};
use dowel_eval::{Document, Site, Value};
use dowel_support::diag::closest;
use dowel_support::{log, Diagnostic, FileId, SourceMap, Span};
use dowel_support::{log_debug, log_trace};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_NAME: &str = "dowel.toml";
pub const BUILD_NAME: &str = "dowel.build";

pub struct Session {
    pub sm: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
    pub packages: Vec<Package>,
    pub targets: Vec<Target>,
    /// 正規化したパッケージルート → 識別子。同じパッケージを2度読まないため。
    by_root: BTreeMap<PathBuf, PackageId>,
}

impl Session {
    /// `root` にあるパッケージと、そこから到達する `path` 依存を読み込む。
    pub fn load(root: &Path) -> Session {
        let _phase = log::Phase::start("load");
        let mut sess = Session {
            sm: SourceMap::new(),
            diagnostics: Vec::new(),
            packages: Vec::new(),
            targets: Vec::new(),
            by_root: BTreeMap::new(),
        };
        let mut queue = vec![canonical(root)];
        while let Some(dir) = queue.pop() {
            if sess.by_root.contains_key(&dir) {
                continue;
            }
            let Some(id) = sess.load_package(&dir) else { continue };
            for dep in sess.packages[id.0].deps.clone() {
                if let DepKind::Path(rel) = &dep.kind {
                    queue.push(canonical(&dir.join(rel)));
                }
            }
        }
        log_debug!(
            "パッケージ {} 件、ターゲット {} 件を読み込んだ",
            sess.packages.len(),
            sess.targets.len()
        );
        sess
    }

    fn load_package(&mut self, dir: &Path) -> Option<PackageId> {
        let manifest_path = dir.join(MANIFEST_NAME);
        let manifest_file = match self.sm.load(&manifest_path) {
            Ok(f) => f,
            Err(e) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "missing-manifest",
                        format!("{} を読めない: {e}", manifest_path.display()),
                    )
                    .note("パッケージのルートには `dowel.toml` が要る"),
                );
                return None;
            }
        };

        let id = PackageId(self.packages.len());
        let doc = self.parse_and_eval(manifest_file, true);
        let mut diags = Vec::new();
        let mut pkg =
            package::from_document(id, &doc, dir.to_path_buf(), manifest_file, &mut diags);
        self.diagnostics.append(&mut diags);

        let build_path = dir.join(BUILD_NAME);
        if build_path.exists() {
            match self.sm.load(&build_path) {
                Ok(f) => {
                    pkg.build_file = Some(f);
                    let doc = self.parse_and_eval(f, false);
                    self.by_root.insert(dir.to_path_buf(), id);
                    self.packages.push(pkg);
                    self.build_targets(id, &doc);
                    log_debug!(
                        "パッケージ `{}` を {} から読み込んだ",
                        self.packages[id.0].name,
                        dir.display()
                    );
                    return Some(id);
                }
                Err(e) => self.diagnostics.push(Diagnostic::error(
                    "unreadable-build",
                    format!("{} を読めない: {e}", build_path.display()),
                )),
            }
        } else {
            self.diagnostics.push(
                Diagnostic::error("missing-build", format!("{} がない", build_path.display()))
                    .note("ターゲット定義は `dowel.build` に置く（docs/10-manifest.md）"),
            );
        }

        self.by_root.insert(dir.to_path_buf(), id);
        self.packages.push(pkg);
        Some(id)
    }

    fn parse_and_eval(&mut self, file: FileId, strict: bool) -> Document {
        let src = self.sm.text(file).to_string();
        let parsed = dowel_syntax::parse(&src, file);
        self.diagnostics.extend(parsed.diagnostics);
        if strict {
            self.diagnostics.extend(dowel_eval::strict::check(&parsed.root, file));
        }
        let (doc, diags) = dowel_eval::eval(&parsed.root, &src, file);
        self.diagnostics.extend(diags);
        doc
    }

    /// `dowel.build` の各テーブルをターゲットへ組み上げる。
    fn build_targets(&mut self, pkg: PackageId, doc: &Document) {
        // `[lib.foo]` と `[lib.foo.public]` は別テーブルだが同じターゲットを指す。
        let mut index: BTreeMap<(String, String), TargetId> = BTreeMap::new();

        for table in &doc.tables {
            if table.path.is_empty() {
                if let Some(first) = table.entries.first() {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "toplevel-entry",
                            "`dowel.build` の最上位にキーは置けない",
                        )
                        .at(first.site.file, first.site.span, "テーブル見出しの中に書く")
                        .note("`[lib.<名前>]` のようにターゲットを宣言してから書く"),
                    );
                }
                continue;
            }

            let Some(kind) = self.parse_kind(table, doc.file) else { continue };
            if table.path.len() < 2 {
                self.diagnostics.push(
                    Diagnostic::error(
                        "missing-target-name",
                        format!("`[{}]` にターゲット名がない", table.path.join(".")),
                    )
                    .at(
                        doc.file,
                        table.site.span,
                        format!("`[{}.<名前>]` と書く", kind.name()),
                    ),
                );
                continue;
            }
            let name = table.path[1].clone();

            let block = match table.path.len() {
                2 => Block::Root,
                3 => {
                    match Block::parse(&table.path[2]) {
                        Some(b) => b,
                        None => {
                            let mut d = Diagnostic::error(
                            "unknown-block",
                            format!("未知のブロック `{}`", table.path[2]),
                        )
                        .at(doc.file, table.site.span, "`public` か `private` のみ")
                        .note("伝播するものとしないものを構文上分離する（docs/10-manifest.md 2節）");
                            if let Some(c) = closest(&table.path[2], ["public", "private"]) {
                                d = d.suggest(
                                    doc.file,
                                    table.site.span,
                                    format!("[{}.{}.{}]", kind.name(), name, c),
                                    format!("`{c}` の誤りではないか"),
                                );
                            }
                            self.diagnostics.push(d);
                            continue;
                        }
                    }
                }
                _ => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "too-deep-table",
                            format!("`[{}]` は深すぎる", table.path.join(".")),
                        )
                        .at(
                            doc.file,
                            table.site.span,
                            "`[種別.名前]` か `[種別.名前.ブロック]`",
                        ),
                    );
                    continue;
                }
            };

            let key = (kind.name().to_string(), name.clone());
            let tid = *index.entry(key).or_insert_with(|| {
                let tid = TargetId(self.targets.len());
                self.targets.push(Target {
                    id: tid,
                    package: pkg,
                    kind,
                    name: name.clone(),
                    site: table.site,
                    root: PropMap::new(),
                    public: PropMap::new(),
                    private: PropMap::new(),
                });
                tid
            });

            for entry in &table.entries {
                self.assign_prop(tid, block, entry.key.clone(), entry.value.clone(), entry.site);
            }
        }

        for t in &self.targets {
            if t.package == pkg {
                log_trace!("ターゲット {}.{} を宣言", t.kind.name(), t.name);
            }
        }
    }

    fn parse_kind(&mut self, table: &dowel_eval::Table, file: FileId) -> Option<TableKind> {
        let head = &table.path[0];
        let Some(kind) = TableKind::parse(head) else {
            let known: Vec<&str> = TableKind::ALL.iter().map(|k| k.name()).collect();
            let mut d = Diagnostic::error("unknown-kind", format!("未知のテーブル種別 `{head}`"))
                .at(file, table.site.span, "この種別はない")
                .note(format!("使えるのは {}", known.join(", ")));
            if let Some(c) = closest(head, known) {
                d = d.suggest(file, table.site.span, c, format!("`{c}` の誤りではないか"));
            }
            self.diagnostics.push(d);
            return None;
        };
        if !kind.is_implemented() {
            self.diagnostics.push(
                Diagnostic::error(
                    "unimplemented-kind",
                    format!("`{}` はまだ実装していない", kind.name()),
                )
                .at(file, table.site.span, "型としては認識しているが処理できない")
                .note("実装済みなのは lib / bin / test"),
            );
            return None;
        }
        Some(kind)
    }

    /// スキーマに照らしてプロパティを検査し、ターゲットへ格納する。
    fn assign_prop(
        &mut self,
        tid: TargetId,
        block: Block,
        key: Vec<String>,
        value: Value,
        site: Site,
    ) {
        // `[lib.foo]` の中に `public.includes = ...` と書く形も許す。
        let (block, name) = match key.len() {
            1 => (block, key[0].clone()),
            2 if block == Block::Root => match Block::parse(&key[0]) {
                Some(b) => (b, key[1].clone()),
                None => (block, key.join(".")),
            },
            _ => (block, key.join(".")),
        };

        let Some(def) = schema::lookup(block, &name) else {
            let known = schema::prop_names(block);
            let mut d = Diagnostic::error("unknown-property", format!("未知のプロパティ `{name}`"))
                .at(
                    site.file,
                    site.span,
                    format!("`{}` にこの名前のプロパティはない", block.name()),
                )
                .note(format!("`{}` に置けるのは {}", block.name(), known.join(", ")));
            if let Some(c) = closest(&name, known.iter().copied()) {
                d = d.suggest(site.file, site.span, c, format!("`{c}` の誤りではないか"));
            } else if let Some(other) = other_block_with(&name, block) {
                d = d.note(format!("`{name}` は `{}` のプロパティである", other.name()));
            }
            self.diagnostics.push(d);
            return;
        };

        if !def.ty.accepts(&value.ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "type-mismatch",
                    format!(
                        "`{name}` は {} だが {} が与えられた",
                        def.ty.display(),
                        value.ty.display()
                    ),
                )
                .at(site.file, site.span, format!("この値の型は {}", value.ty.display()))
                .note(path_hint(&def.ty, &value.ty)),
            );
            return;
        }

        let target = &mut self.targets[tid.0];
        if let Some(prev) = target.props(block).get(&name) {
            let prev_site = prev.prov.nearest_site();
            let mut d = Diagnostic::error(
                "duplicate-property",
                format!("`{name}` が同じブロックで2度指定されている"),
            )
            .at(site.file, site.span, "2度目の指定");
            if let Some(s) = prev_site {
                d = d.with_label(dowel_support::Label::secondary(s.file, s.span, "最初の指定"));
            }
            self.diagnostics.push(d);
            return;
        }
        target.props_mut(block).insert(name, value);
    }

    pub fn package(&self, id: PackageId) -> &Package {
        &self.packages[id.0]
    }

    /// 依存名から読み込み済みパッケージを引く。
    /// 取得を要する供給形態（レジストリ / git）は未実装のため `None` になる。
    pub fn dep_package(&self, from: PackageId, dep_name: &str) -> Option<PackageId> {
        let pkg = self.package(from);
        let dep = pkg.deps.iter().find(|d| d.name == dep_name)?;
        match &dep.kind {
            DepKind::Path(rel) => self.by_root.get(&canonical(&pkg.root.join(rel))).copied(),
            DepKind::Unsupported(_) => None,
        }
    }

    pub fn target(&self, id: TargetId) -> &Target {
        &self.targets[id.0]
    }

    pub fn label(&self, id: TargetId) -> String {
        let t = self.target(id);
        label(&self.package(t.package).name, &t.name)
    }

    /// `pkg:name` または `name` でターゲットを引く。
    /// 名前のみの指定は、一意に定まる場合に限り受け付ける。
    pub fn find_target(&self, spec: &str) -> Result<TargetId, String> {
        if let Some((pkg, name)) = spec.split_once(':') {
            return self
                .targets
                .iter()
                .find(|t| t.name == name && self.package(t.package).name == pkg)
                .map(|t| t.id)
                .ok_or_else(|| format!("ターゲット `{spec}` が見つからない"));
        }
        let matches: Vec<&Target> = self.targets.iter().filter(|t| t.name == spec).collect();
        match matches.len() {
            0 => {
                let all: Vec<String> = self.targets.iter().map(|t| self.label(t.id)).collect();
                Err(format!(
                    "ターゲット `{spec}` が見つからない。存在するのは {}",
                    if all.is_empty() { "（なし）".to_string() } else { all.join(", ") }
                ))
            }
            1 => Ok(matches[0].id),
            _ => {
                let labels: Vec<String> = matches.iter().map(|t| self.label(t.id)).collect();
                Err(format!(
                    "`{spec}` は複数のパッケージにある: {}。`パッケージ名:{spec}` と書く",
                    labels.join(", ")
                ))
            }
        }
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == dowel_support::Severity::Error)
    }

    /// 根のパッケージ。最初に読み込んだもの。
    pub fn root_package(&self) -> Option<&Package> {
        self.packages.first()
    }
}

/// 同じ名前が別ブロックに存在するか。診断の注記に使う。
fn other_block_with(name: &str, block: Block) -> Option<Block> {
    [Block::Root, Block::Public, Block::Private]
        .into_iter()
        .find(|&b| b != block && schema::lookup(b, name).is_some())
}

/// 型不一致のうち、頻出するものに具体的な助言を返す。
fn path_hint(expected: &dowel_eval::Type, actual: &dowel_eval::Type) -> String {
    use dowel_eval::Type;
    let wants_path = matches!(expected.elem().unwrap_or(expected), Type::Path);
    let gives_str = matches!(actual.elem().unwrap_or(actual), Type::Str);
    if wants_path && gives_str {
        "パスは文字列から作らない。`dir(\"...\")` / `file(\"...\")` / `glob(\"...\")` を使う".into()
    } else {
        format!("期待する型は {}", expected.display())
    }
}

/// 正規化に失敗しても落とさない。存在しないパスを指す診断は後段で出す。
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// 位置を持たない診断のための空スパン。
pub const NO_SPAN: Span = Span::EMPTY;
