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

/// 値の入れ子の深さの既定の上限。`--max-nesting` で変えられる。
///
/// 解析は再帰下降であり、深さがそのままスタックを使う。上限が無いと
/// 生成された入力で abort し、診断を1件も出せない（issue #33）。
/// 言語仕様は停止性を保証すると定めており（ADR-0004 の帰結）、処理系が
/// 入力の深さで落ちるのはその主張と食い違う。実在のマニフェストの
/// 入れ子は数段であり、64 は人が書く形に対して十分に深い。
pub const MAX_NESTING: usize = 64;

/// 指定できる上限の天井。
///
/// 上限は「abort しない」ことを守るための仕掛けであり、スタックが尽きる
/// 深さまで上げられては意味を失う。観測では 6000 段のあたりで溢れた
/// （issue #33）ため、余裕を持って1桁下に置く。
pub const MAX_NESTING_CEILING: usize = 512;

pub fn parse(src: &str, file: FileId) -> Parsed {
    parse_with_max_nesting(src, file, MAX_NESTING)
}

/// 入れ子の上限を与えて解析する。`--max-nesting` の配管の終点。
///
/// 呼び手は [`MAX_NESTING_CEILING`] 以下に検証してから渡す。
pub fn parse_with_max_nesting(src: &str, file: FileId, max_nesting: usize) -> Parsed {
    let lexed = lex(src);
    let mut p = Parser {
        src,
        file,
        tokens: lexed.tokens,
        pos: 0,
        depth: 0,
        max_nesting: max_nesting.min(MAX_NESTING_CEILING),
        builder: TreeBuilder::new(),
        diagnostics: Vec::new(),
    };
    for e in lexed.errors {
        let d = match e.kind {
            LexErrorKind::UnterminatedString => Diagnostic::error(
                "unterminated-string",
                "unterminated string",
            )
            .at(file, e.span, "the string opened here is never closed"),
            LexErrorKind::UnterminatedBlockComment => Diagnostic::error(
                "unterminated-comment",
                "unterminated block comment",
            )
            .at(file, e.span, "expected `*/`"),
            LexErrorKind::UnknownChar => Diagnostic::error(
                "unknown-char",
                "unrecognized character",
            )
            .at(file, e.span, "this character cannot appear here"),
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
    /// いま解析している値の入れ子の深さ。`max_nesting` で打ち切る。
    depth: usize,
    /// 入れ子の上限。既定は [`MAX_NESTING`]
    max_nesting: usize,
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
        *self.tokens.last().expect("an Eof token is always present")
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
            Diagnostic::error("expected-token", format!("expected {}", kind.describe())).at(
                self.file,
                found.span,
                format!("found {} instead", found.kind.describe()),
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
                    "entries must be separated by a newline",
                    "this entry starts on the same line as the previous one",
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
                        format!("{} cannot appear here", t.kind.describe()),
                        "expected a table header `[...]` or a key",
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
                    "expected a key",
                    "write an identifier or a quoted string",
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
        self.pred_or();
        self.builder.finish_node();
    }

    /// `a or b`。優先順位は `not` > `and` > `or`（ADR-0032）。
    ///
    /// 演算子は語であって記号ではない。周りの言語の演算子（`when` / `match`
    /// / `glob`）が語であり、`&&` は別の言語を差し込んだように読める。
    ///
    /// どの段でも**改行を跨がない**。`when` 自身が跨がないのと同じ理由による
    /// ——次の行の鍵がたまたま `or` だったときに、それを演算子として食べる。
    fn pred_or(&mut self) {
        self.skip_trivia();
        let cp = self.builder.checkpoint();
        self.pred_and();
        while !self.newline_before_next() && self.at_keyword(0, "or") {
            self.builder.start_node_at(cp, NodeKind::PredOr);
            self.bump(); // `or`
            self.pred_and();
            self.builder.finish_node();
        }
    }

    fn pred_and(&mut self) {
        self.skip_trivia();
        let cp = self.builder.checkpoint();
        self.pred_unary();
        while !self.newline_before_next() && self.at_keyword(0, "and") {
            self.builder.start_node_at(cp, NodeKind::PredAnd);
            self.bump(); // `and`
            self.pred_unary();
            self.builder.finish_node();
        }
    }

    fn pred_unary(&mut self) {
        self.skip_trivia();
        if self.at_keyword(0, "not") {
            let at = self.nth_token(0).span.start;
            self.builder.start_node(NodeKind::PredNot, at);
            self.bump(); // `not`
            self.pred_unary();
            self.builder.finish_node();
            return;
        }
        self.pred_atom();
    }

    fn pred_atom(&mut self) {
        self.skip_trivia();
        let at = self.nth_token(0).span.start;
        self.builder.start_node(NodeKind::PredAtom, at);
        if self.nth(0) == TokenKind::LParen {
            self.bump(); // `(`
            self.pred_or();
            self.skip_trivia();
            if self.nth(0) == TokenKind::RParen {
                self.bump();
            } else {
                let t = self.nth_token(0);
                self.err_at(
                    t.span,
                    "expected-close",
                    "expected `)` to close the predicate",
                    "write `)`",
                );
                self.recover(&[TokenKind::Comma, TokenKind::RBracket]);
            }
            self.builder.finish_node();
            return;
        }
        self.ns_ref();
        if self.nth(0) == TokenKind::EqEq {
            self.bump();
            match self.nth(0) {
                TokenKind::Str | TokenKind::Int | TokenKind::Ident => self.literal(),
                _ => {
                    let t = self.nth_token(0);
                    self.err_at(
                        t.span,
                        "expected-value",
                        "expected the right-hand side of the comparison",
                        "write a string",
                    );
                    self.recover(&[TokenKind::Comma, TokenKind::RBracket]);
                }
            }
        }
        self.builder.finish_node();
    }

    fn expr(&mut self) {
        self.skip_trivia();
        // 深さの検査は全ての値の再帰がここを通ることに依っている。
        // 配列・インラインテーブル・呼び出し・`match` の腕は、いずれも
        // `expr`（または `expr_with_when` 経由）で子の値へ降りる。
        // 数えるのは入れ物の段数である。リテラルは再帰しないため、
        // 上限ちょうどの入れ物の中身までは受け付ける。
        let opens_a_container = matches!(self.nth(0), TokenKind::LBracket | TokenKind::LBrace)
            || (self.nth(0) == TokenKind::Ident
                && (self.at_keyword(0, "match") || self.nth(1) == TokenKind::LParen));
        if opens_a_container && self.depth >= self.max_nesting {
            self.too_deep();
            return;
        }
        self.depth += 1;
        self.expr_inner();
        self.depth -= 1;
    }

    /// 深さの上限を超えた値。診断を1件出し、値の残りを再帰せずに読み切る。
    ///
    /// 読み切りは括弧の釣り合いだけを数える反復であり、深さに依存しない。
    /// 上限を超えた部分木は1つの [`NodeKind::Error`] になる。
    fn too_deep(&mut self) {
        let t = self.nth_token(0);
        self.diagnostics.push(
            Diagnostic::error(
                "nesting-too-deep",
                format!("the value is nested more than {} levels deep", self.max_nesting),
            )
            .at(self.file, t.span, "the nesting reaches its limit here")
            .note("such depth usually comes from a generated manifest")
            .note("flatten the value, or raise the limit with `--max-nesting`"),
        );
        self.builder.start_node(NodeKind::Error, t.span.start);
        let mut open = 0usize;
        while let Some(t) = self.tokens.get(self.pos).copied() {
            match t.kind {
                TokenKind::Eof => break,
                TokenKind::LBracket | TokenKind::LBrace | TokenKind::LParen => open += 1,
                TokenKind::RBracket | TokenKind::RBrace | TokenKind::RParen if open == 0 => break,
                TokenKind::RBracket | TokenKind::RBrace | TokenKind::RParen => open -= 1,
                // 釣り合いの取れた位置の区切りは、囲んでいる要素の区切りである。
                TokenKind::Comma | TokenKind::Newline if open == 0 => break,
                _ => {}
            }
            self.builder.token(t);
            self.pos += 1;
        }
        self.builder.finish_node();
    }

    fn expr_inner(&mut self) {
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
                    format!("expected a value but found {}", t.kind.describe()),
                    "one of: string, integer, boolean, array, inline table, function call, `match`",
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
                    self.err_at(
                        t.span,
                        "expected-name",
                        "expected a name after `.`",
                        "write a name",
                    );
                    break;
                }
            }
        } else {
            let t = self.nth_token(0);
            self.err_at(
                t.span,
                "expected-name",
                "expected a name",
                "write a reference such as `cfg.opt`",
            );
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
                    self.err_at(
                        t.span,
                        "expected-token",
                        "expected `,` or `)`",
                        "argument separator",
                    );
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
                    self.err_at(
                        t.span,
                        "expected-token",
                        "expected `,` or `]`",
                        "element separator",
                    );
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
                    self.err_at(
                        t.span,
                        "expected-token",
                        "expected `,` or `}`",
                        "element separator",
                    );
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
                    self.err_at(
                        t.span,
                        "expected-token",
                        "expected `,` or `}`",
                        "match arm separator",
                    );
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
                    "expected the left-hand side of a match arm",
                    "a possible value, or `_`",
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
        assert_eq!(parsed.root.text(src), src, "the CST cannot reproduce its input");
    }

    #[test]
    fn parses_a_target_definition() {
        let src = "[lib.foo]\nsources = glob(\"src/**.c\")\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_lossless(src);
        let tree = parsed.root.debug_tree(src);
        assert!(tree.contains("TableHeader"), "{tree}");
        assert!(tree.contains("Call"), "{tree}");
    }

    #[test]
    fn distinguishes_array_table_headers() {
        let src = "[[dependencies]]\nname = \"zlib\"\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.root.nodes().next().unwrap().kind, NodeKind::ArrayTableHeader);
        assert_lossless(src);
    }

    #[test]
    fn parses_a_match_expression() {
        let src = "flags = match cfg.opt {\n  debug   => [\"-O0\", \"-g3\"],\n  release => [\"-O2\"],\n}\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let kv = parsed.root.child(NodeKind::KeyValue).unwrap();
        let m = kv.child(NodeKind::Match).unwrap();
        assert_eq!(m.children_of(NodeKind::MatchArm).count(), 2);
        assert_lossless(src);
    }

    #[test]
    fn postfix_when_wraps_the_expression() {
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
    fn postfix_when_also_attaches_to_a_key() {
        let src = "flags = [\"-fsanitize=address\"] when feature.asan\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let kv = parsed.root.child(NodeKind::KeyValue).unwrap();
        assert!(kv.child(NodeKind::WhenExpr).is_some(), "{}", parsed.root.debug_tree(src));
        assert_lossless(src);
    }

    #[test]
    fn postfix_when_does_not_cross_a_newline() {
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
    fn a_predicate_binds_not_tighter_than_and_tighter_than_or() {
        // `a or b and c` は `a or (b and c)` である（ADR-0032）。
        let src =
            "flags = [\"-x\"] when cfg.opt == \"debug\" or target.os == \"linux\" and feature.z\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let clause = parsed
            .root
            .child(NodeKind::KeyValue)
            .unwrap()
            .child(NodeKind::WhenExpr)
            .unwrap()
            .child(NodeKind::WhenClause)
            .unwrap();
        // 根は `or`、その右側が `and`。逆に畳んでいれば根が `and` になる。
        let or = clause.child(NodeKind::PredOr).expect("the root should be `or`");
        assert!(or.child(NodeKind::PredAnd).is_some(), "{}", parsed.root.debug_tree(src));
        assert_lossless(src);
    }

    #[test]
    fn parentheses_override_the_precedence() {
        let src = "flags = [\"-x\"] when (cfg.opt == \"debug\" or target.os == \"linux\") and feature.z\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let clause = parsed
            .root
            .child(NodeKind::KeyValue)
            .unwrap()
            .child(NodeKind::WhenExpr)
            .unwrap()
            .child(NodeKind::WhenClause)
            .unwrap();
        // 今度は根が `and`。
        assert!(clause.child(NodeKind::PredAnd).is_some(), "{}", parsed.root.debug_tree(src));
        assert!(clause.child(NodeKind::PredOr).is_none());
        assert_lossless(src);
    }

    #[test]
    fn not_binds_to_one_atom() {
        let src = "flags = [\"-x\"] when not target.os == \"windows\" and feature.z\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let clause = parsed
            .root
            .child(NodeKind::KeyValue)
            .unwrap()
            .child(NodeKind::WhenExpr)
            .unwrap()
            .child(NodeKind::WhenClause)
            .unwrap();
        // `(not a) and b` であり、`not (a and b)` ではない。
        let and = clause.child(NodeKind::PredAnd).expect("the root should be `and`");
        assert!(and.child(NodeKind::PredNot).is_some(), "{}", parsed.root.debug_tree(src));
        assert_lossless(src);
    }

    #[test]
    fn a_predicate_does_not_cross_a_newline_either() {
        // `when` 自身と同じ規則。次の行の鍵がたまたま `or` のとき、
        // それを演算子として食べてはならない。
        let src = "flags = [\"-x\"] when feature.z\nor      = \"yes\"\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.root.children_of(NodeKind::KeyValue).count(), 2);
        assert_lossless(src);
    }

    #[test]
    fn parses_an_inline_table() {
        let src = "defines = { FOO_BUILDING = 1, BAR = \"x\" }\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let t =
            parsed.root.child(NodeKind::KeyValue).unwrap().child(NodeKind::InlineTable).unwrap();
        assert_eq!(t.children_of(NodeKind::KeyValue).count(), 2);
        assert_lossless(src);
    }

    #[test]
    fn keeps_parsing_after_an_error() {
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
    fn unterminated_array_is_still_reproduced() {
        let src = "sources = [\"a.c\", \"b.c\"\n";
        let parsed = p(src);
        assert!(!parsed.diagnostics.is_empty());
        assert_lossless(src);
    }

    #[test]
    fn unterminated_table_header_is_still_reproduced() {
        let src = "[lib.foo\nsources = []\n";
        let parsed = p(src);
        assert!(!parsed.diagnostics.is_empty());
        assert_lossless(src);
    }

    #[test]
    fn entries_on_one_line_are_diagnosed() {
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
    fn comments_and_blank_lines_are_kept_in_the_tree() {
        let src = "# header description\n\n[lib.foo]  # trailing comment\n";
        assert_lossless(src);
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }

    #[test]
    fn handles_empty_input() {
        assert_lossless("");
        assert!(p("").diagnostics.is_empty());
    }

    #[test]
    fn terminates_on_a_run_of_errors() {
        // 前進の保証（recover が必ず1トークン消費する）の検査。
        let src = "@ @ @ ] } ) , = =>\n@@@\n";
        let parsed = p(src);
        assert!(!parsed.diagnostics.is_empty());
        assert_lossless(src);
    }

    #[test]
    fn a_leading_bom_is_accepted() {
        // 画面に見えない違いで拒むと、続く診断が「正しく見える行」を指す
        // （issue #34）。CRLF と同様、先頭の BOM は受け付ける。
        let src = "\u{feff}[lib.foo]\nsources = glob(\"src/*.c\")\n";
        let parsed = p(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_lossless(src);
    }

    #[test]
    fn nesting_below_the_limit_is_untouched() {
        let deep = format!("a = {}1{}\n", "[".repeat(MAX_NESTING), "]".repeat(MAX_NESTING));
        let parsed = p(&deep);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_lossless(&deep);
    }

    #[test]
    fn nesting_beyond_the_limit_is_refused_with_a_location() {
        // 上限が無いと再帰がスタックを溢れさせ、診断を1件も出せない
        // （issue #33）。拒否は abort ではなく、位置を持つ診断で行う。
        let deep = format!("a = {}1{}\n", "[".repeat(MAX_NESTING + 1), "]".repeat(MAX_NESTING + 1));
        let parsed = p(&deep);
        let d = parsed
            .diagnostics
            .iter()
            .find(|d| d.code == "nesting-too-deep")
            .expect("the depth limit did not report");
        let label = d.primary_label().expect("the diagnostic carries no location");
        // 位置は上限に達した括弧を指す。
        assert_eq!(label.span.start as usize, 4 + MAX_NESTING);
        assert_lossless(&deep);
    }

    #[test]
    fn the_limit_is_configurable() {
        // 生成されたマニフェストが 64 を超える場合の逃げ道（PR #36 のレビュー）。
        let deep = format!("a = {}1{}\n", "[".repeat(100), "]".repeat(100));
        let parsed = parse_with_max_nesting(&deep, FileId(0), 128);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

        // 天井は越えられない。上限は abort させないための仕掛けであり、
        // スタックが尽きる深さを受け付けては意味を失う。
        let very_deep = format!("a = {}1{}\n", "[".repeat(600), "]".repeat(600));
        let parsed = parse_with_max_nesting(&very_deep, FileId(0), usize::MAX);
        assert!(parsed.diagnostics.iter().any(|d| d.code == "nesting-too-deep"));
    }

    #[test]
    fn extreme_nesting_does_not_abort() {
        // 生成された入力の形（issue #33 の観測）。深さに再帰が比例しないこと。
        // 閉じた形・閉じていない形・呼び出しの形の3つとも見る。
        for src in [
            format!("a = {}1{}\n", "[".repeat(100_000), "]".repeat(100_000)),
            format!("a = {}\n", "[".repeat(100_000)),
            format!("a = {}{{a=1}}{}\n", "{a=".repeat(50_000), "}".repeat(50_000)),
            format!("a = {}\"x\"{}\n", "glob(".repeat(50_000), ")".repeat(50_000)),
        ] {
            let parsed = p(&src);
            assert!(
                parsed.diagnostics.iter().any(|d| d.code == "nesting-too-deep"),
                "no depth diagnostic for {}...",
                &src[..20]
            );
            assert_lossless(&src);
        }
    }
}
