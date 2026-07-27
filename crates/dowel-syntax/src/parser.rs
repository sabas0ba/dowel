//! 誤り耐性のあるパーサ。
//!
//! 構文誤りで停止せず、誤りの部分木（[`NodeKind::Error`]）を残して解析を続ける
//! （docs/20-architecture.md 2節の制約1）。言語サーバは常に部分的な木を得られ、
//! CLI は1回の実行で全ての構文誤りを報告できる。
//!
//! 文法は TOML の構造をそのまま用い、値の位置にのみ式を許す（[ADR-0004]）。
//!
//! [ADR-0004]: ../../../docs/adr/0004-syntax.md

use crate::cst::{Node, NodeKind, TreeBuilder};
use crate::lexer::{lex, LexErrorKind, Token, TokenKind};
use dowel_support::{Diagnostic, FileId, Span};

pub struct Parsed {
    pub root: Node,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(src: &str, file: FileId) -> Parsed {
    let lexed = lex(src);
    let mut p = Parser {
        src,
        file,
        tokens: lexed.tokens,
        pos: 0,
        builder: TreeBuilder::new(),
        diagnostics: Vec::new(),
    };
    for e in lexed.errors {
        let d = match e.kind {
            LexErrorKind::UnterminatedString => Diagnostic::error(
                "unterminated-string",
                "文字列が閉じられていない",
            )
            .at(file, e.span, "ここで始まった文字列に対応する引用符がない"),
            LexErrorKind::UnterminatedBlockComment => Diagnostic::error(
                "unterminated-comment",
                "ブロックコメントが閉じられていない",
            )
            .at(file, e.span, "`*/` が必要"),
            LexErrorKind::UnknownChar => Diagnostic::error(
                "unknown-char",
                "認識できない文字がある",
            )
            .at(file, e.span, "この位置に置ける文字ではない"),
        };
        p.diagnostics.push(d);
    }
    p.document();
    Parsed { root: p.builder.finish(), diagnostics: p.diagnostics }
}

struct Parser<'a> {
    src: &'a str,
    file: FileId,
    tokens: Vec<Token>,
    /// `tokens` の位置。些末部を含む生の位置である。
    pos: usize,
    builder: TreeBuilder,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    // ---- トークン列の操作 ------------------------------------------------

    /// `n` 番目の些末部でないトークン。先読みのみで消費しない。
    fn nth_token(&self, n: usize) -> Token {
        let mut seen = 0;
        for t in &self.tokens[self.pos..] {
            if t.kind.is_trivia() {
                continue;
            }
            if seen == n {
                return *t;
            }
            seen += 1;
        }
        *self.tokens.last().expect("Eof が必ず存在する")
    }

    fn nth(&self, n: usize) -> TokenKind {
        self.nth_token(n).kind
    }

    fn nth_text(&self, n: usize) -> &'a str {
        &self.src[self.nth_token(n).span.range()]
    }

    fn at_keyword(&self, n: usize, kw: &str) -> bool {
        self.nth(n) == TokenKind::Ident && self.nth_text(n) == kw
    }

    /// 次の些末部でないトークンまでの間に改行があるか。
    ///
    /// 後置 `when` の判定に要る。改行を跨いで拾うと、次の行のキーが
    /// たまたま `when` だった場合（`dowel.toml` の条件付き依存がまさにそれ）に
    /// 前の行の値へ吸い込まれる。
    fn newline_before_next(&self) -> bool {
        self.tokens[self.pos..]
            .iter()
            .take_while(|t| t.kind.is_trivia())
            .any(|t| t.kind == TokenKind::Newline)
    }

    /// 些末部を木へ積みながら読み飛ばす。改行を跨いだ場合に `true`。
    fn skip_trivia(&mut self) -> bool {
        let mut saw_newline = false;
        while let Some(t) = self.tokens.get(self.pos) {
            if !t.kind.is_trivia() {
                break;
            }
            saw_newline |= t.kind == TokenKind::Newline;
            self.builder.token(*t);
            self.pos += 1;
        }
        saw_newline
    }

    fn bump(&mut self) {
        self.skip_trivia();
        if let Some(t) = self.tokens.get(self.pos) {
            if t.kind != TokenKind::Eof {
                self.builder.token(*t);
                self.pos += 1;
            }
        }
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.nth(0) == kind {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let found = self.nth_token(0);
        self.diagnostics.push(
            Diagnostic::error("expected-token", format!("{} が必要", kind.describe())).at(
                self.file,
                found.span,
                format!("{} が現れた", found.kind.describe()),
            ),
        );
        false
    }

    fn err_at(&mut self, span: Span, code: &'static str, msg: impl Into<String>, label: &str) {
        self.diagnostics.push(Diagnostic::error(code, msg).at(self.file, span, label));
    }

    /// 誤りの部分木を作り、区切りまで読み飛ばす。
    ///
    /// 必ず1トークン以上を消費する。呼び出し側のループが前進しないことを防ぐため、
    /// この保証はここに置く。
    fn recover(&mut self, stops: &[TokenKind]) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::Error, at);
        let mut consumed = 0;
        while let Some(t) = self.tokens.get(self.pos).copied() {
            if t.kind == TokenKind::Eof {
                break;
            }
            if consumed > 0 && (t.kind == TokenKind::Newline || stops.contains(&t.kind)) {
                break;
            }
            self.builder.token(t);
            self.pos += 1;
            consumed += 1;
        }
        self.builder.finish_node();
    }

    // ---- 文法 ------------------------------------------------------------

    fn document(&mut self) {
        let mut parsed_item = false;
        loop {
            let saw_newline = self.skip_trivia();
            let kind = self.nth(0);
            if kind == TokenKind::Eof {
                break;
            }
            // TOML では項目が行で区切られる。値が自己完結するため構文上は
            // 区切りがなくても解析できるが、TOML の期待から外れるため診断する
            // （docs/10-manifest.md 6節）。
            if parsed_item && !saw_newline {
                let span = self.nth_token(0).span;
                self.err_at(
                    span,
                    "missing-newline",
                    "項目は行で区切る",
                    "直前の項目と同じ行に次の項目がある",
                );
            }
            match kind {
                TokenKind::LBracket => self.table_header(),
                TokenKind::Ident | TokenKind::Str => self.key_value(),
                _ => {
                    let t = self.nth_token(0);
                    self.err_at(
                        t.span,
                        "unexpected-token",
                        format!("{} はここに置けない", t.kind.describe()),
                        "テーブル見出し `[...]` かキーが必要",
                    );
                    self.recover(&[]);
                }
            }
            parsed_item = true;
        }
    }

    /// `[lib.foo]` および `[[dependencies]]`
    fn table_header(&mut self) {
        let array = self.nth(1) == TokenKind::LBracket;
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder
            .start_node(if array { NodeKind::ArrayTableHeader } else { NodeKind::TableHeader }, at);
        self.bump(); // `[`
        if array {
            self.bump(); // 2つ目の `[`
        }
        self.key_path();
        self.expect(TokenKind::RBracket);
        if array {
            self.expect(TokenKind::RBracket);
        }
        self.builder.finish_node();
    }

    fn key_path(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::KeyPath, at);
        self.key_segment();
        while self.nth(0) == TokenKind::Dot {
            self.bump();
            self.key_segment();
        }
        self.builder.finish_node();
    }

    fn key_segment(&mut self) {
        match self.nth(0) {
            TokenKind::Ident | TokenKind::Str => self.bump(),
            _ => {
                let t = self.nth_token(0);
                self.err_at(
                    t.span,
                    "expected-key",
                    "キーが必要",
                    "識別子または引用符つき文字列を置く",
                );
                self.recover(&[TokenKind::RBracket, TokenKind::Eq, TokenKind::Dot]);
            }
        }
    }

    fn key_value(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::KeyValue, at);
        self.key_path();
        if self.expect(TokenKind::Eq) {
            self.expr_with_when();
        } else {
            self.recover(&[]);
        }
        self.builder.finish_node();
    }

    /// 式と、それに後置される `when`。
    ///
    /// `when` を後置にしたのは、配列要素とキーの双方に一様に付けられるため
    /// （[ADR-0004] の帰結）。解析上は既に積んだ式を包み直す必要があり、
    /// これが `TreeBuilder::checkpoint` の存在理由である。
    ///
    /// [ADR-0004]: ../../../docs/adr/0004-syntax.md
    fn expr_with_when(&mut self) {
        self.skip_trivia();
        let cp = self.builder.checkpoint();
        self.expr();
        if !self.newline_before_next() && self.at_keyword(0, "when") {
            self.builder.start_node_at(cp, NodeKind::WhenExpr);
            self.when_clause();
            self.builder.finish_node();
        }
    }

    fn when_clause(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::WhenClause, at);
        self.bump(); // `when`
        self.ns_ref();
        if self.nth(0) == TokenKind::EqEq {
            self.bump();
            match self.nth(0) {
                TokenKind::Str | TokenKind::Int | TokenKind::Ident => self.literal(),
                _ => {
                    let t = self.nth_token(0);
                    self.err_at(t.span, "expected-value", "比較の右辺が必要", "文字列を置く");
                    self.recover(&[TokenKind::Comma, TokenKind::RBracket]);
                }
            }
        }
        self.builder.finish_node();
    }

    fn expr(&mut self) {
        self.skip_trivia();
        match self.nth(0) {
            TokenKind::Str | TokenKind::Int => self.literal(),
            TokenKind::Ident => {
                if self.at_keyword(0, "match") {
                    self.match_expr();
                } else if self.at_keyword(0, "true") || self.at_keyword(0, "false") {
                    self.literal();
                } else if self.nth(1) == TokenKind::LParen {
                    self.call();
                } else {
                    self.ns_ref();
                }
            }
            TokenKind::LBracket => self.array(),
            TokenKind::LBrace => self.inline_table(),
            _ => {
                let t = self.nth_token(0);
                self.err_at(
                    t.span,
                    "expected-value",
                    format!("値が必要だが {} が現れた", t.kind.describe()),
                    "文字列・整数・真偽値・配列・インラインテーブル・関数呼び出し・`match` のいずれか",
                );
                self.recover(&[
                    TokenKind::Comma,
                    TokenKind::RBracket,
                    TokenKind::RBrace,
                    TokenKind::RParen,
                ]);
            }
        }
    }

    fn literal(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::Literal, at);
        self.bump();
        self.builder.finish_node();
    }

    /// `cfg.opt`, `feature.zlib`, `host.os`
    fn ns_ref(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::NsRef, at);
        if self.nth(0) == TokenKind::Ident {
            self.bump();
            while self.nth(0) == TokenKind::Dot {
                self.bump();
                if self.nth(0) == TokenKind::Ident {
                    self.bump();
                } else {
                    let t = self.nth_token(0);
                    self.err_at(t.span, "expected-name", "`.` の後に名前が必要", "名前を置く");
                    break;
                }
            }
        } else {
            let t = self.nth_token(0);
            self.err_at(t.span, "expected-name", "名前が必要", "`cfg.opt` のような参照を置く");
            self.recover(&[TokenKind::LBrace, TokenKind::Comma, TokenKind::RBracket]);
        }
        self.builder.finish_node();
    }

    /// `glob("src/**.c")`, `dir("include")`, `dep("bar")`, `target("foo")`
    fn call(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::Call, at);
        self.bump(); // 関数名
        self.expect(TokenKind::LParen);
        loop {
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::RParen | TokenKind::Eof => break,
                _ => {}
            }
            self.expr();
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::Comma => self.bump(),
                TokenKind::RParen | TokenKind::Eof => break,
                _ => {
                    let t = self.nth_token(0);
                    self.err_at(t.span, "expected-token", "`,` か `)` が必要", "引数の区切り");
                    self.recover(&[TokenKind::Comma, TokenKind::RParen]);
                }
            }
        }
        self.expect(TokenKind::RParen);
        self.builder.finish_node();
    }

    fn array(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::Array, at);
        self.bump(); // `[`
        loop {
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::RBracket | TokenKind::Eof => break,
                _ => {}
            }
            self.expr_with_when();
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::Comma => self.bump(),
                TokenKind::RBracket | TokenKind::Eof => break,
                _ => {
                    let t = self.nth_token(0);
                    self.err_at(t.span, "expected-token", "`,` か `]` が必要", "要素の区切り");
                    self.recover(&[TokenKind::Comma, TokenKind::RBracket]);
                }
            }
        }
        self.expect(TokenKind::RBracket);
        self.builder.finish_node();
    }

    fn inline_table(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::InlineTable, at);
        self.bump(); // `{`
        loop {
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::RBrace | TokenKind::Eof => break,
                _ => {}
            }
            let kv_at = self.nth_token(0).span.start;
            self.builder.start_node(NodeKind::KeyValue, kv_at);
            self.key_path();
            if self.expect(TokenKind::Eq) {
                self.expr_with_when();
            }
            self.builder.finish_node();
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::Comma => self.bump(),
                TokenKind::RBrace | TokenKind::Eof => break,
                _ => {
                    let t = self.nth_token(0);
                    self.err_at(t.span, "expected-token", "`,` か `}` が必要", "要素の区切り");
                    self.recover(&[TokenKind::Comma, TokenKind::RBrace]);
                }
            }
        }
        self.expect(TokenKind::RBrace);
        self.builder.finish_node();
    }

    /// `match cfg.opt { debug => [...], release => [...] }`
    fn match_expr(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::Match, at);
        self.bump(); // `match`
        self.ns_ref();
        self.expect(TokenKind::LBrace);
        loop {
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::RBrace | TokenKind::Eof => break,
                _ => {}
            }
            self.match_arm();
            self.skip_trivia();
            match self.nth(0) {
                TokenKind::Comma => self.bump(),
                TokenKind::RBrace | TokenKind::Eof => break,
                _ => {
                    let t = self.nth_token(0);
                    self.err_at(t.span, "expected-token", "`,` か `}` が必要", "アームの区切り");
                    self.recover(&[TokenKind::Comma, TokenKind::RBrace]);
                }
            }
        }
        self.expect(TokenKind::RBrace);
        self.builder.finish_node();
    }

    fn match_arm(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::MatchArm, at);

        let pat_at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::Pattern, pat_at);
        match self.nth(0) {
            TokenKind::Ident | TokenKind::Str => self.bump(),
            _ => {
                let t = self.nth_token(0);
                self.err_at(
                    t.span,
                    "expected-pattern",
                    "アームの左辺が必要",
                    "取りうる値の名前、または `_`",
                );
                self.recover(&[TokenKind::FatArrow, TokenKind::Comma, TokenKind::RBrace]);
            }
        }
        self.builder.finish_node();

        if self.expect(TokenKind::FatArrow) {
            self.expr();
        }
        self.builder.finish_node();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Parsed {
        parse(src, FileId(0))
    }

    /// CST がロスレスであること。全ての木の検査の前提になる。
    fn assert_lossless(src: &str) {
        let parsed = p(src);
        assert_eq!(parsed.root.text(src), src, "CST が入力を復元できない");
    }

    #[test]
    fn ターゲット定義を解析する() {
        let src = "[lib.foo]\nsources = glob(\"src/**.c\")\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_lossless(src);
        let tree = parsed.root.debug_tree(src);
        assert!(tree.contains("TableHeader"), "{tree}");
        assert!(tree.contains("Call"), "{tree}");
    }

    #[test]
    fn 配列テーブル見出しを区別する() {
        let src = "[[dependencies]]\nname = \"zlib\"\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.root.nodes().next().unwrap().kind, NodeKind::ArrayTableHeader);
        assert_lossless(src);
    }

    #[test]
    fn match_式を解析する() {
        let src = "flags = match cfg.opt {\n  debug   => [\"-O0\", \"-g3\"],\n  release => [\"-O2\"],\n}\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let kv = parsed.root.child(NodeKind::KeyValue).unwrap();
        let m = kv.child(NodeKind::Match).unwrap();
        assert_eq!(m.children_of(NodeKind::MatchArm).count(), 2);
        assert_lossless(src);
    }

    #[test]
    fn 後置の_when_は式を包む() {
        let src = "deps = [dep(\"zlib\") when feature.zlib]\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let arr = parsed.root.child(NodeKind::KeyValue).unwrap().child(NodeKind::Array).unwrap();
        let elem = arr.child(NodeKind::WhenExpr).unwrap();
        assert!(elem.child(NodeKind::Call).is_some());
        assert!(elem.child(NodeKind::WhenClause).is_some());
        assert_lossless(src);
    }

    #[test]
    fn 後置の_when_はキーにも付く() {
        let src = "flags = [\"-fsanitize=address\"] when feature.asan\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let kv = parsed.root.child(NodeKind::KeyValue).unwrap();
        assert!(kv.child(NodeKind::WhenExpr).is_some(), "{}", parsed.root.debug_tree(src));
        assert_lossless(src);
    }

    #[test]
    fn 後置の_when_は改行を跨がない() {
        // 次の行のキーが `when` である場合（dowel.toml の条件付き依存）に、
        // 前の行の値へ吸い込まれてはならない。
        let src = "version = \"0.2\"\nwhen    = { os = \"windows\" }\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.root.children_of(NodeKind::KeyValue).count(), 2);
        assert!(parsed.root.child(NodeKind::WhenExpr).is_none());
        assert_lossless(src);
    }

    #[test]
    fn インラインテーブルを解析する() {
        let src = "defines = { FOO_BUILDING = 1, BAR = \"x\" }\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let t =
            parsed.root.child(NodeKind::KeyValue).unwrap().child(NodeKind::InlineTable).unwrap();
        assert_eq!(t.children_of(NodeKind::KeyValue).count(), 2);
        assert_lossless(src);
    }

    #[test]
    fn 誤りがあっても後続の項目を解析する() {
        let src = "[lib.foo]\nsources = @@@\nincludes = [dir(\"include\")]\n";
        let parsed = p(src);
        assert!(!parsed.diagnostics.is_empty());
        assert_lossless(src);
        // 2つ目の key-value は健全に解析されている。
        let kvs: Vec<_> = parsed.root.children_of(NodeKind::KeyValue).collect();
        assert_eq!(kvs.len(), 2);
        assert!(!kvs[1].has_error(), "{}", parsed.root.debug_tree(src));
    }

    #[test]
    fn 閉じない配列でも復元できる() {
        let src = "sources = [\"a.c\", \"b.c\"\n";
        let parsed = p(src);
        assert!(!parsed.diagnostics.is_empty());
        assert_lossless(src);
    }

    #[test]
    fn 閉じないテーブル見出しでも復元できる() {
        let src = "[lib.foo\nsources = []\n";
        let parsed = p(src);
        assert!(!parsed.diagnostics.is_empty());
        assert_lossless(src);
    }

    #[test]
    fn 同じ行に項目を並べると診断する() {
        let src = "a = 1 b = 2\n";
        let parsed = p(src);
        assert!(
            parsed.diagnostics.iter().any(|d| d.code == "missing-newline"),
            "{:?}",
            parsed.diagnostics
        );
        assert_lossless(src);
    }

    #[test]
    fn コメントと空行を木に保持する() {
        let src = "# 見出しの説明\n\n[lib.foo]  # 末尾コメント\n";
        assert_lossless(src);
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }

    #[test]
    fn 空入力を扱える() {
        assert_lossless("");
        assert!(p("").diagnostics.is_empty());
    }

    #[test]
    fn 誤りの連続でも停止する() {
        // 前進の保証（recover が必ず1トークン消費する）の検査。
        let src = "@ @ @ ] } ) , = =>\n@@@\n";
        let parsed = p(src);
        assert!(!parsed.diagnostics.is_empty());
        assert_lossless(src);
    }
}
