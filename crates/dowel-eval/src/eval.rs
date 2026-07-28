//! CST から値への評価。
//!
//! 式は純粋かつ全域である。副作用なし、束縛なし、再帰なし（[ADR-0004]）。
//! `glob` のファイル走査すらここでは行わない。評価時に走査すると、
//! その時点のファイルシステムという**記録されない入力**が評価結果に混ざる。
//!
//! [ADR-0004]: ../../../docs/adr/0004-syntax.md

use crate::config::{domain_of, known_keys, Domain};
use crate::value::{
    CfgKey, Data, MatchArm, Ns, Origin, PathBase, PathValue, Pattern, Pred, Prov, Site, Type, Value,
};
use dowel_support::diag::closest;
use dowel_support::{Diagnostic, FileId, Span};
use dowel_syntax::{Node, NodeKind, TokenKind};
use std::collections::BTreeMap;

/// 評価済みのマニフェスト1ファイル。
pub struct Document {
    pub file: FileId,
    pub tables: Vec<Table>,
}

pub struct Table {
    /// `[lib.foo.public]` なら `["lib", "foo", "public"]`
    pub path: Vec<String>,
    /// `[[dependencies]]` 形式か
    pub array: bool,
    pub site: Site,
    pub entries: Vec<Entry>,
}

pub struct Entry {
    /// ドット付きキーを許すため列で持つ
    pub key: Vec<String>,
    pub site: Site,
    pub value: Value,
}

impl Document {
    /// 先頭が `prefix` に一致するテーブル。
    pub fn tables_under<'a>(&'a self, prefix: &'a [&str]) -> impl Iterator<Item = &'a Table> {
        self.tables.iter().filter(move |t| {
            t.path.len() >= prefix.len() && t.path.iter().zip(prefix).all(|(a, b)| a.as_str() == *b)
        })
    }

    pub fn table(&self, path: &[&str]) -> Option<&Table> {
        self.tables
            .iter()
            .find(|t| t.path.len() == path.len() && t.path.iter().zip(path).all(|(a, b)| a == b))
    }
}

impl Table {
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key.len() == 1 && e.key[0] == key)
    }
}

pub fn eval(root: &Node, src: &str, file: FileId) -> (Document, Vec<Diagnostic>) {
    let mut ev = Evaluator { src, file, diags: Vec::new() };
    let mut tables: Vec<Table> = Vec::new();
    // 見出しの前に現れた key-value は根のテーブルに属する。
    tables.push(Table {
        path: Vec::new(),
        array: false,
        site: Site::new(file, Span::EMPTY),
        entries: Vec::new(),
    });

    for node in root.nodes() {
        match node.kind {
            NodeKind::TableHeader | NodeKind::ArrayTableHeader => {
                let array = node.kind == NodeKind::ArrayTableHeader;
                let path = ev.key_path(node);
                let site = Site::new(file, node.span);
                if !array {
                    if let Some(prev) = tables.iter().find(|t| t.path == path && !t.array) {
                        ev.diags.push(
                            Diagnostic::error(
                                "duplicate-table",
                                format!("duplicate table `[{}]`", path.join(".")),
                            )
                            .at(file, node.span, "defined again here")
                            .with_label(
                                dowel_support::Label::secondary(
                                    file,
                                    prev.site.span,
                                    "first defined here",
                                ),
                            ),
                        );
                    }
                }
                tables.push(Table { path, array, site, entries: Vec::new() });
            }
            NodeKind::KeyValue => {
                let entry = ev.key_value(node);
                let table = tables.last_mut().expect("the root table always exists");
                if let Some(prev) = table.entries.iter().find(|e| e.key == entry.key) {
                    ev.diags.push(
                        Diagnostic::error(
                            "duplicate-key",
                            format!("duplicate key `{}`", entry.key.join(".")),
                        )
                        .at(file, entry.site.span, "set again here")
                        .with_label(dowel_support::Label::secondary(
                            file,
                            prev.site.span,
                            "first set here",
                        )),
                    );
                }
                table.entries.push(entry);
            }
            NodeKind::Error => {}
            _ => {}
        }
    }

    // 空の根テーブルは下流の判定を煩わせるだけなので落とす。
    if tables[0].entries.is_empty() {
        tables.remove(0);
    }
    (Document { file, tables }, ev.diags)
}

struct Evaluator<'a> {
    src: &'a str,
    file: FileId,
    diags: Vec<Diagnostic>,
}

impl<'a> Evaluator<'a> {
    fn text(&self, span: Span) -> &'a str {
        &self.src[span.range()]
    }

    fn site(&self, span: Span) -> Site {
        Site::new(self.file, span)
    }

    fn err(&mut self, span: Span, code: &'static str, msg: impl Into<String>, label: &str) {
        self.diags.push(Diagnostic::error(code, msg).at(self.file, span, label));
    }

    /// `[lib.foo.public]` の見出しからキー列を取り出す。
    fn key_path(&mut self, header: &Node) -> Vec<String> {
        let Some(kp) = header.child(NodeKind::KeyPath) else { return Vec::new() };
        self.segments(kp)
    }

    fn segments(&mut self, key_path: &Node) -> Vec<String> {
        let mut out = Vec::new();
        for t in key_path.tokens() {
            match t.kind {
                TokenKind::Ident => out.push(self.text(t.span).to_string()),
                TokenKind::Str => {
                    let s = self.string_literal(t.span);
                    out.push(s);
                }
                _ => {}
            }
        }
        out
    }

    fn key_value(&mut self, node: &Node) -> Entry {
        let key = node.child(NodeKind::KeyPath).map(|kp| self.segments(kp)).unwrap_or_default();
        let value = match node.nodes().find(|n| n.kind != NodeKind::KeyPath) {
            Some(v) => self.expr(v),
            None => Value::error(Prov::at(Origin::Literal, self.site(node.span))),
        };
        Entry { key, site: self.site(node.span), value }
    }

    fn expr(&mut self, node: &Node) -> Value {
        match node.kind {
            NodeKind::Literal => self.literal(node),
            NodeKind::Array => self.array(node),
            NodeKind::InlineTable => self.inline_table(node),
            NodeKind::Call => self.call(node),
            NodeKind::Match => self.match_expr(node),
            NodeKind::WhenExpr => self.when_expr(node),
            NodeKind::NsRef => {
                let name = self.ns_text(node);
                self.err(
                    node.span,
                    "unexpected-reference",
                    format!("`{name}` cannot appear in a value position"),
                    "configuration references belong in a `match` scrutinee or a `when` predicate",
                );
                Value::error(Prov::at(Origin::Literal, self.site(node.span)))
            }
            NodeKind::Error => Value::error(Prov::at(Origin::Literal, self.site(node.span))),
            _ => Value::error(Prov::at(Origin::Literal, self.site(node.span))),
        }
    }

    fn literal(&mut self, node: &Node) -> Value {
        let prov = Prov::at(Origin::Literal, self.site(node.span));
        let Some(t) = node.tokens().find(|t| !t.kind.is_trivia()) else {
            return Value::error(prov);
        };
        match t.kind {
            TokenKind::Str => {
                let s = self.string_literal(t.span);
                Value { ty: Type::Str, data: Data::Str(s), prov }
            }
            TokenKind::Int => match parse_int(self.text(t.span)) {
                Some(i) => Value { ty: Type::Int, data: Data::Int(i), prov },
                None => {
                    self.err(
                        t.span,
                        "invalid-integer",
                        "not a readable integer",
                        "check the digits",
                    );
                    Value::error(prov)
                }
            },
            TokenKind::Ident => {
                let text = self.text(t.span);
                match text {
                    "true" => Value { ty: Type::Bool, data: Data::Bool(true), prov },
                    "false" => Value { ty: Type::Bool, data: Data::Bool(false), prov },
                    _ => Value::error(prov),
                }
            }
            _ => Value::error(prov),
        }
    }

    /// 引用符つきトークンから文字列の中身を取り出す。
    fn string_literal(&mut self, span: Span) -> String {
        let raw = self.text(span);
        let bytes = raw.as_bytes();
        let quote = bytes.first().copied().unwrap_or(b'"');
        let triple = bytes.len() >= 6 && bytes[1] == quote && bytes[2] == quote;
        let delim = if triple { 3 } else { 1 };
        let end = raw.len().saturating_sub(if raw.len() >= delim * 2 { delim } else { 0 });
        let body = raw.get(delim..end).unwrap_or("");
        // 複数行文字列では、開始区切りの直後の改行を落とす（TOML の規則）。
        let body = if triple { body.strip_prefix('\n').unwrap_or(body) } else { body };
        if quote == b'\'' {
            // リテラル文字列はエスケープを解釈しない。
            return body.to_string();
        }
        let mut out = String::with_capacity(body.len());
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(c) => out.push(c),
                        None => {
                            self.err(
                                span,
                                "invalid-escape",
                                format!("`\\u{hex}` is not a character"),
                                "write a code point as four hexadecimal digits",
                            );
                        }
                    }
                }
                Some(other) => {
                    self.err(
                        span,
                        "invalid-escape",
                        format!("`\\{other}` is not a recognized escape"),
                        "supported escapes are \\n \\t \\r \\0 \\\\ \\\" \\uXXXX",
                    );
                    out.push(other);
                }
                None => {}
            }
        }
        out
    }

    fn array(&mut self, node: &Node) -> Value {
        let prov = Prov::at(Origin::Literal, self.site(node.span));
        let items: Vec<Value> = node.nodes().map(|n| self.expr(n)).collect();
        let ty = unify_elems(&items);
        Value { ty: Type::List(Box::new(ty)), data: Data::List(items), prov }
    }

    fn inline_table(&mut self, node: &Node) -> Value {
        let prov = Prov::at(Origin::Literal, self.site(node.span));
        let mut map = BTreeMap::new();
        for kv in node.children_of(NodeKind::KeyValue) {
            let entry = self.key_value(kv);
            let key = entry.key.join(".");
            if map.contains_key(&key) {
                self.err(
                    kv.span,
                    "duplicate-key",
                    format!("duplicate key `{key}`"),
                    "a key may appear only once per table",
                );
                continue;
            }
            map.insert(key, entry.value);
        }
        let ty = unify_elems(&map.values().cloned().collect::<Vec<_>>());
        Value { ty: Type::Map(Box::new(ty)), data: Data::Map(map), prov }
    }

    fn call(&mut self, node: &Node) -> Value {
        let name_tok = node.tokens().find(|t| t.kind == TokenKind::Ident);
        let name = name_tok.map(|t| self.text(t.span)).unwrap_or("");
        let site = self.site(node.span);
        let prov = Prov::at(Origin::Call(name.to_string()), site);
        let args: Vec<Value> = node.nodes().map(|n| self.expr(n)).collect();

        const FUNCTIONS: &[&str] = &["glob", "dir", "file", "dep", "target"];
        if !FUNCTIONS.contains(&name) {
            let mut d = Diagnostic::error("unknown-function", format!("unknown function `{name}`"))
                .at(self.file, node.span, "no function has this name")
                .note(format!("available functions: {}", FUNCTIONS.join(", ")));
            if let Some(c) = closest(name, FUNCTIONS.iter().copied()) {
                if let Some(t) = name_tok {
                    d = d.suggest(self.file, t.span, c, format!("did you mean `{c}`?"));
                }
            }
            self.diags.push(d);
            return Value::error(prov);
        }

        // いずれの関数も引数は文字列1つ。
        if args.len() != 1 {
            self.err(
                node.span,
                "wrong-arity",
                format!("`{name}` takes one argument but {} were given", args.len()),
                "wrong number of arguments",
            );
            return Value::error(prov);
        }
        let Some(arg) = args[0].as_str().map(|s| s.to_string()) else {
            if !args[0].is_error() {
                self.err(
                    node.span,
                    "type-mismatch",
                    format!("the argument of `{name}` must be a string"),
                    "pass a string",
                );
            }
            return Value::error(prov);
        };

        match name {
            // glob の展開は plan 時。評価時に走査すると記録されない入力が混ざる。
            "glob" => Value { ty: Type::List(Box::new(Type::Path)), data: Data::Glob(arg), prov },
            "dir" | "file" => Value {
                ty: Type::Path,
                data: Data::Path(PathValue { base: PathBase::Package, rel: normalize_rel(&arg) }),
                prov,
            },
            "dep" => Value { ty: Type::DepRef, data: Data::Dep(arg), prov },
            "target" => Value { ty: Type::TargetRef, data: Data::Target(arg), prov },
            _ => unreachable!("unknown functions are rejected above"),
        }
    }

    fn ns_text(&self, node: &Node) -> String {
        let mut parts = Vec::new();
        for t in node.tokens() {
            if t.kind == TokenKind::Ident {
                parts.push(self.text(t.span));
            }
        }
        parts.join(".")
    }

    /// 名前空間参照を検証してキーにする。
    fn cfg_key(&mut self, node: &Node) -> Option<CfgKey> {
        let parts: Vec<&str> = node
            .tokens()
            .filter(|t| t.kind == TokenKind::Ident)
            .map(|t| self.text(t.span))
            .collect();
        if parts.len() != 2 {
            self.err(
                node.span,
                "invalid-reference",
                format!("`{}` is not a valid configuration reference", parts.join(".")),
                "write a namespace and a name, as in `cfg.opt`",
            );
            return None;
        }
        let Some(ns) = Ns::parse(parts[0]) else {
            let known = ["cfg", "host", "feature", "tc"];
            let mut d =
                Diagnostic::error("unknown-namespace", format!("unknown namespace `{}`", parts[0]))
                    .at(self.file, node.span, "no such namespace")
                    .note(format!("available namespaces: {}", known.join(", ")));
            if let Some(c) = closest(parts[0], known) {
                d = d.suggest(
                    self.file,
                    node.span,
                    format!("{c}.{}", parts[1]),
                    format!("did you mean `{c}`?"),
                );
            }
            self.diags.push(d);
            return None;
        };
        let key = CfgKey { ns, name: parts[1].to_string() };
        if domain_of(&key).is_none() {
            let known = known_keys(ns);
            let mut d = Diagnostic::error(
                "unknown-cfg-key",
                format!("unknown configuration key `{}`", key.display()),
            )
            .at(self.file, node.span, "this name is not in the vocabulary")
            .note(format!("`{}` accepts: {}", ns.name(), known.join(", ")))
            .note("the vocabulary is provisional; see Q1 in docs/99-open-questions.md");
            if let Some(c) = closest(
                &key.name,
                known_keys(ns).iter().map(|s| {
                    // `cfg.opt` から `opt` を取り出して比較する
                    let s: &str = s;
                    s.rsplit('.').next().unwrap_or(s)
                }),
            ) {
                d = d.suggest(
                    self.file,
                    node.span,
                    format!("{}.{c}", ns.name()),
                    format!("did you mean `{c}`?"),
                );
            }
            self.diags.push(d);
            return None;
        }
        Some(key)
    }

    fn match_expr(&mut self, node: &Node) -> Value {
        let site = self.site(node.span);
        let Some(ns) = node.child(NodeKind::NsRef) else {
            return Value::error(Prov::at(Origin::Literal, site));
        };
        let Some(key) = self.cfg_key(ns) else {
            return Value::error(Prov::at(Origin::Literal, site));
        };

        let mut arms: Vec<MatchArm> = Vec::new();
        for arm_node in node.children_of(NodeKind::MatchArm) {
            let Some(pat_node) = arm_node.child(NodeKind::Pattern) else { continue };
            let pattern = match pat_node.tokens().find(|t| !t.kind.is_trivia()) {
                Some(t) if t.kind == TokenKind::Str => Pattern::Value(self.string_literal(t.span)),
                Some(t) if t.kind == TokenKind::Ident => {
                    let text = self.text(t.span);
                    if text == "_" {
                        Pattern::Wildcard
                    } else {
                        Pattern::Value(text.to_string())
                    }
                }
                _ => continue,
            };
            let value = match arm_node.nodes().find(|n| n.kind != NodeKind::Pattern) {
                Some(v) => self.expr(v),
                None => Value::error(Prov::at(Origin::Literal, self.site(arm_node.span))),
            };
            if arms.iter().any(|a| a.pattern == pattern) {
                self.err(
                    arm_node.span,
                    "duplicate-arm",
                    format!("duplicate match arm `{}`", pattern.display()),
                    "one arm per value",
                );
                continue;
            }
            arms.push(MatchArm { pattern, value, site: self.site(arm_node.span) });
        }

        self.check_exhaustive(&key, &arms, node.span);

        let inner = unify_elems(&arms.iter().map(|a| a.value.clone()).collect::<Vec<_>>());
        Value {
            ty: Type::Cfg(Box::new(inner)),
            data: Data::Match { scrutinee: key, arms },
            prov: Prov::at(Origin::Literal, site),
        }
    }

    /// 網羅性検査。
    ///
    /// 停止性の保証（[ADR-0004]）と同じ理由でここに置く。実行時に「どのアームにも
    /// 該当しない」が起きうる状態を残さない。
    ///
    /// [ADR-0004]: ../../../docs/adr/0004-syntax.md
    fn check_exhaustive(&mut self, key: &CfgKey, arms: &[MatchArm], span: Span) {
        let has_wildcard = arms.iter().any(|a| a.pattern == Pattern::Wildcard);
        let Some(domain) = domain_of(key) else { return };
        match domain {
            Domain::Finite(values) => {
                for a in arms {
                    if let Pattern::Value(v) = &a.pattern {
                        if !values.contains(&v.as_str()) {
                            let mut d = Diagnostic::error(
                                "unknown-pattern",
                                format!("`{}` is not a possible value of `{}`", v, key.display()),
                            )
                            .at(self.file, a.site.span, "this value is not in the vocabulary")
                            .note(format!("possible values: {}", values.join(", ")));
                            if let Some(c) = closest(v, values.iter().copied()) {
                                d = d.suggest(
                                    self.file,
                                    a.site.span,
                                    c,
                                    format!("did you mean `{c}`?"),
                                );
                            }
                            self.diags.push(d);
                        }
                    }
                }
                if has_wildcard {
                    return;
                }
                let missing: Vec<&str> = values
                    .iter()
                    .copied()
                    .filter(|v| {
                        !arms.iter().any(|a| matches!(&a.pattern, Pattern::Value(p) if p == v))
                    })
                    .collect();
                if !missing.is_empty() {
                    self.diags.push(
                        Diagnostic::error(
                            "non-exhaustive-match",
                            format!("non-exhaustive match on `{}`", key.display()),
                        )
                        .at(self.file, span, format!("missing: {}", missing.join(", ")))
                        .note("add `_ => ...`, or an arm for each missing value"),
                    );
                }
            }
            Domain::Bool => {
                if !has_wildcard {
                    let covered: Vec<&str> = ["true", "false"]
                        .into_iter()
                        .filter(|v| {
                            arms.iter().any(|a| matches!(&a.pattern, Pattern::Value(p) if p == v))
                        })
                        .collect();
                    if covered.len() < 2 {
                        self.diags.push(
                            Diagnostic::error(
                                "non-exhaustive-match",
                                format!("non-exhaustive match on `{}`", key.display()),
                            )
                            .at(self.file, span, "both true and false are required")
                            .note("`_ => ...` works too"),
                        );
                    }
                }
            }
            Domain::Open => {
                if !has_wildcard {
                    self.diags.push(
                        Diagnostic::error(
                            "non-exhaustive-match",
                            format!("`{}` has an open domain", key.display()),
                        )
                        .at(self.file, span, "`_ => ...` is required")
                        .note("free-form strings such as target triples cannot be enumerated"),
                    );
                }
            }
        }
    }

    fn when_expr(&mut self, node: &Node) -> Value {
        let site = self.site(node.span);
        let Some(clause) = node.child(NodeKind::WhenClause) else {
            return Value::error(Prov::at(Origin::Literal, site));
        };
        let inner = match node.nodes().find(|n| n.kind != NodeKind::WhenClause) {
            Some(n) => self.expr(n),
            None => Value::error(Prov::at(Origin::Literal, site)),
        };
        let Some(pred) = self.pred(clause) else {
            return Value::error(Prov::at(Origin::Literal, site));
        };
        Value {
            ty: Type::Cfg(Box::new(inner.ty.clone())),
            data: Data::When { pred, inner: Box::new(inner) },
            prov: Prov::at(Origin::Literal, site),
        }
    }

    fn pred(&mut self, clause: &Node) -> Option<Pred> {
        let ns = clause.child(NodeKind::NsRef)?;
        let key = self.cfg_key(ns)?;
        let rhs = clause.child(NodeKind::Literal);
        match rhs {
            None => {
                // 真偽として読む。feature 以外は値域が真偽でないため比較を要求する。
                if !matches!(domain_of(&key), Some(Domain::Bool)) {
                    self.err(
                        clause.span,
                        "expected-comparison",
                        format!("`{}` is not a boolean", key.display()),
                        "compare it with `== \"...\"`",
                    );
                    return None;
                }
                Some(Pred::Flag(key))
            }
            Some(lit) => {
                let v = self.literal(lit);
                let Some(s) = v.as_str() else {
                    if !v.is_error() {
                        self.err(
                            lit.span,
                            "type-mismatch",
                            "the right-hand side must be a string",
                            "write a string",
                        );
                    }
                    return None;
                };
                if let Some(Domain::Finite(values)) = domain_of(&key) {
                    if !values.contains(&s) {
                        let mut d = Diagnostic::error(
                            "unknown-pattern",
                            format!("`{}` is not a possible value of `{}`", s, key.display()),
                        )
                        .at(self.file, lit.span, "this value is not in the vocabulary")
                        .note(format!("possible values: {}", values.join(", ")));
                        if let Some(c) = closest(s, values.iter().copied()) {
                            d = d.suggest(
                                self.file,
                                lit.span,
                                format!("{c:?}"),
                                format!("did you mean `{c}`?"),
                            );
                        }
                        self.diags.push(d);
                    }
                }
                Some(Pred::Eq(key, s.to_string()))
            }
        }
    }
}

/// 要素の型を1つに統一する。統一できない場合は `Unknown` とし、
/// 型検査は代入先のスキーマが行う。
fn unify_elems(items: &[Value]) -> Type {
    let mut ty: Option<Type> = None;
    for v in items {
        let t = v.ty.concrete().clone();
        if matches!(t, Type::Unknown) {
            continue;
        }
        match &ty {
            None => ty = Some(t),
            Some(prev) if *prev == t => {}
            Some(_) => return Type::Unknown,
        }
    }
    ty.unwrap_or(Type::Unknown)
}

fn parse_int(text: &str) -> Option<i64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    let (sign, rest) = match cleaned.strip_prefix('-') {
        Some(r) => (-1i64, r.to_string()),
        None => (1i64, cleaned.trim_start_matches('+').to_string()),
    };
    let v = if let Some(h) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()?
    } else if let Some(o) = rest.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()?
    } else if let Some(b) = rest.strip_prefix("0b") {
        i64::from_str_radix(b, 2).ok()?
    } else {
        rest.parse::<i64>().ok()?
    };
    Some(sign * v)
}

/// パスの区切りを `/` に寄せ、`./` を落とす。
fn normalize_rel(rel: &str) -> String {
    let s = rel.replace('\\', "/");
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> (Document, Vec<Diagnostic>) {
        let parsed = dowel_syntax::parse(src, FileId(0));
        assert!(parsed.diagnostics.is_empty(), "syntax errors: {:?}", parsed.diagnostics);
        eval(&parsed.root, src, FileId(0))
    }

    fn codes(diags: &[Diagnostic]) -> Vec<&str> {
        diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn extracts_tables_and_keys() {
        let (doc, diags) = run("[lib.foo]\nsources = glob(\"src/*.c\")\n\n[lib.foo.public]\nincludes = [dir(\"include\")]\n");
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(doc.tables.len(), 2);
        assert_eq!(doc.tables[0].path, vec!["lib", "foo"]);
        assert_eq!(doc.tables[1].path, vec!["lib", "foo", "public"]);
        let sources = doc.table(&["lib", "foo"]).unwrap().entry("sources").unwrap();
        assert_eq!(sources.value.data, Data::Glob("src/*.c".into()));
        assert_eq!(sources.value.ty, Type::List(Box::new(Type::Path)));
    }

    #[test]
    fn interprets_string_escapes() {
        let (doc, diags) = run("a = \"x\\ny\"\nb = 'x\\ny'\nc = \"\"\"\nmulti\n\"\"\"\n");
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        let t = &doc.tables[0];
        assert_eq!(t.entry("a").unwrap().value.as_str(), Some("x\ny"));
        assert_eq!(
            t.entry("b").unwrap().value.as_str(),
            Some("x\\ny"),
            "literal strings are not interpreted"
        );
        assert_eq!(t.entry("c").unwrap().value.as_str(), Some("multi\n"));
    }

    #[test]
    fn integer_notations() {
        let (doc, diags) = run("a = 1_000\nb = 0xff\nc = -3\n");
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        let t = &doc.tables[0];
        assert_eq!(t.entry("a").unwrap().value.as_int(), Some(1000));
        assert_eq!(t.entry("b").unwrap().value.as_int(), Some(255));
        assert_eq!(t.entry("c").unwrap().value.as_int(), Some(-3));
    }

    #[test]
    fn path_is_a_distinct_type_from_str() {
        let (doc, _) = run("a = dir(\"include\")\nb = \"include\"\n");
        let t = &doc.tables[0];
        assert_eq!(t.entry("a").unwrap().value.ty, Type::Path);
        assert_eq!(t.entry("b").unwrap().value.ty, Type::Str);
    }

    #[test]
    fn suggests_a_candidate_for_an_unknown_function() {
        let (_, diags) = run("a = glab(\"src\")\n");
        assert_eq!(codes(&diags), vec!["unknown-function"]);
        assert_eq!(diags[0].suggestions[0].replacement, "glob");
    }

    #[test]
    fn checks_argument_count() {
        let (_, diags) = run("a = dir(\"x\", \"y\")\n");
        assert_eq!(codes(&diags), vec!["wrong-arity"]);
    }

    #[test]
    fn non_exhaustive_match_fails() {
        let (_, diags) = run("flags = match cfg.opt { debug => [\"-O0\"] }\n");
        assert_eq!(codes(&diags), vec!["non-exhaustive-match"]);
    }

    #[test]
    fn a_wildcard_makes_a_match_exhaustive() {
        let (_, diags) = run("flags = match cfg.opt { debug => [\"-O0\"], _ => [\"-O2\"] }\n");
        assert!(diags.is_empty(), "{:?}", codes(&diags));
    }

    #[test]
    fn open_domain_cfg_requires_a_wildcard() {
        let (_, diags) =
            run("a = match cfg.target { \"x86_64-unknown-linux-gnu\" => [\"-m64\"] }\n");
        assert_eq!(codes(&diags), vec!["non-exhaustive-match"]);
    }

    #[test]
    fn diagnoses_arms_outside_the_vocabulary() {
        let (_, diags) = run("a = match cfg.opt { debug => [], releaes => [], _ => [] }\n");
        assert_eq!(codes(&diags), vec!["unknown-pattern"]);
        assert_eq!(diags[0].suggestions[0].replacement, "release");
    }

    #[test]
    fn diagnoses_unknown_configuration_keys() {
        let (_, diags) = run("a = match cfg.optimization { _ => [] }\n");
        assert_eq!(codes(&diags), vec!["unknown-cfg-key"]);
    }

    #[test]
    fn when_validates_its_predicate() {
        let (doc, diags) = run("deps = [dep(\"zlib\") when feature.zlib]\n");
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        let v = &doc.tables[0].entry("deps").unwrap().value;
        let elem = &v.as_list().unwrap()[0];
        assert!(matches!(&elem.data, Data::When { pred: Pred::Flag(_), .. }));
    }

    #[test]
    fn non_boolean_when_requires_a_comparison() {
        let (_, diags) = run("deps = [dep(\"zlib\") when cfg.opt]\n");
        assert_eq!(codes(&diags), vec!["expected-comparison"]);
    }

    #[test]
    fn when_validates_the_compared_value() {
        let (_, diags) = run("flags = [\"-g\"] when cfg.opt == \"dbg\"\n");
        assert_eq!(codes(&diags), vec!["unknown-pattern"]);
    }

    #[test]
    fn diagnoses_duplicate_tables_and_keys() {
        let (_, diags) = run("[lib.foo]\na = 1\na = 2\n\n[lib.foo]\nb = 1\n");
        assert_eq!(codes(&diags), vec!["duplicate-key", "duplicate-table"]);
    }

    #[test]
    fn array_tables_are_not_duplicates() {
        let (doc, diags) =
            run("[[dependencies]]\nname = \"a\"\n\n[[dependencies]]\nname = \"b\"\n");
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(doc.tables_under(&["dependencies"]).count(), 2);
    }

    #[test]
    fn values_carry_provenance() {
        let (doc, _) = run("includes = [dir(\"include\")]\n");
        let v = &doc.tables[0].entry("includes").unwrap().value;
        let elem = &v.as_list().unwrap()[0];
        assert!(matches!(elem.prov.origin(), Some(Origin::Call(f)) if f == "dir"));
        assert!(elem.prov.nearest_site().is_some());
    }
}
