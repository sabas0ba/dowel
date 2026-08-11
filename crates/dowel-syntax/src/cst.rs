//! ロスレス具象構文木。
//!
//! 正本は CST であり、AST はその射影とする（docs/20-architecture.md 2節）。
//! 空白・コメント・誤りの部分木を含め、木を辿って連結すれば元のソースに戻る。
//! この性質は言語サーバの整形・書き換えと、`dowel add` のような機械書き換えの前提になる。

use crate::lexer::{Token, TokenKind};
use dowel_support::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    Root,
    /// `[lib.foo]`
    TableHeader,
    /// `[[dependencies]]`
    ArrayTableHeader,
    /// `lib.foo.public`
    KeyPath,
    /// `sources = glob("src/*.c")`
    KeyValue,
    /// `[a, b]`
    Array,
    /// `{ FOO = 1 }`
    InlineTable,
    /// `glob("src/*.c")`
    Call,
    /// `match cfg.opt { ... }`
    Match,
    /// `debug => ["-O0"]`
    MatchArm,
    /// `match` のアーム左辺
    Pattern,
    /// `when feature.zlib`
    WhenClause,
    /// `a or b`（[ADR-0032](../../../docs/adr/0032-predicate-composition.md)）
    PredOr,
    /// `a and b`
    PredAnd,
    /// `not a`
    PredNot,
    /// 述語の葉。`<鍵>`、`<鍵> == "値"`、または括弧で包んだ述語
    PredAtom,
    /// `when` を後置された式。`WhenClause` と被修飾式を子に持つ
    WhenExpr,
    /// `cfg.opt`, `feature.zlib`
    NsRef,
    /// 文字列・整数・真偽値
    Literal,
    /// 誤りを含む部分木。評価は残りを継続する
    Error,
}

#[derive(Clone, Debug)]
pub enum Child {
    Node(Node),
    Token(Token),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub span: Span,
    pub children: Vec<Child>,
}

impl Node {
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.children.iter().filter_map(|c| match c {
            Child::Node(n) => Some(n),
            Child::Token(_) => None,
        })
    }

    pub fn tokens(&self) -> impl Iterator<Item = &Token> {
        self.children.iter().filter_map(|c| match c {
            Child::Token(t) => Some(t),
            Child::Node(_) => None,
        })
    }

    /// 直下の子ノードのうち最初に `kind` に一致するもの。
    pub fn child(&self, kind: NodeKind) -> Option<&Node> {
        self.nodes().find(|n| n.kind == kind)
    }

    pub fn children_of(&self, kind: NodeKind) -> impl Iterator<Item = &Node> {
        self.nodes().filter(move |n| n.kind == kind)
    }

    /// 直下の些末部でないトークンのうち最初に `kind` に一致するもの。
    pub fn token(&self, kind: TokenKind) -> Option<&Token> {
        self.tokens().find(|t| t.kind == kind)
    }

    /// 部分木に誤りノードを含むか。含む場合、下流はこの部分を評価しない。
    pub fn has_error(&self) -> bool {
        self.kind == NodeKind::Error || self.nodes().any(|n| n.has_error())
    }

    /// 部分木を元のテキストに復元する。ロスレス性の検査に使う。
    pub fn text(&self, src: &str) -> String {
        let mut out = String::new();
        self.write_text(src, &mut out);
        out
    }

    fn write_text(&self, src: &str, out: &mut String) {
        for c in &self.children {
            match c {
                Child::Token(t) => out.push_str(&src[t.span.range()]),
                Child::Node(n) => n.write_text(src, out),
            }
        }
    }

    /// 構造の検査とスナップショットのための表示。些末部は省く。
    pub fn debug_tree(&self, src: &str) -> String {
        let mut out = String::new();
        self.write_debug(src, 0, &mut out);
        out
    }

    fn write_debug(&self, src: &str, depth: usize, out: &mut String) {
        out.push_str(&"  ".repeat(depth));
        out.push_str(&format!("{:?}@{:?}\n", self.kind, self.span));
        for c in &self.children {
            match c {
                Child::Token(t) if t.kind.is_trivia() => {}
                Child::Token(t) => {
                    out.push_str(&"  ".repeat(depth + 1));
                    out.push_str(&format!("{:?} {:?}\n", t.kind, &src[t.span.range()]));
                }
                Child::Node(n) => n.write_debug(src, depth + 1, out),
            }
        }
    }
}

/// CST を組み立てる。`checkpoint` により、既に積んだ子を後から
/// 新しいノードで包める（後置の `when` の解析に必要）。
pub struct TreeBuilder {
    stack: Vec<(NodeKind, u32, Vec<Child>)>,
}

/// 「ここまでに積んだ子の位置」。`start_node_at` に渡す。
#[derive(Clone, Copy, Debug)]
pub struct Checkpoint(usize);

impl TreeBuilder {
    pub fn new() -> TreeBuilder {
        TreeBuilder { stack: vec![(NodeKind::Root, 0, Vec::new())] }
    }

    pub fn start_node(&mut self, kind: NodeKind, at: u32) {
        self.stack.push((kind, at, Vec::new()));
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.stack.last().expect("builder stack is empty").2.len())
    }

    /// `cp` 以降に積んだ子を取り出し、`kind` のノードで包み直す。
    pub fn start_node_at(&mut self, cp: Checkpoint, kind: NodeKind) {
        let top = self.stack.last_mut().expect("builder stack is empty");
        let taken: Vec<Child> = top.2.split_off(cp.0);
        let at = taken
            .first()
            .map(|c| match c {
                Child::Node(n) => n.span.start,
                Child::Token(t) => t.span.start,
            })
            .unwrap_or(top.1);
        self.stack.push((kind, at, taken));
    }

    pub fn token(&mut self, t: Token) {
        self.stack.last_mut().expect("builder stack is empty").2.push(Child::Token(t));
    }

    pub fn finish_node(&mut self) {
        let (kind, at, children) =
            self.stack.pop().expect("finish_node without a matching start_node");
        let node = build(kind, at, children);
        self.stack.last_mut().expect("the root node must not be closed").2.push(Child::Node(node));
    }

    pub fn finish(mut self) -> Node {
        assert_eq!(self.stack.len(), 1, "a node was left unclosed");
        let (kind, at, children) = self.stack.pop().unwrap();
        build(kind, at, children)
    }
}

impl Default for TreeBuilder {
    fn default() -> TreeBuilder {
        TreeBuilder::new()
    }
}

fn build(kind: NodeKind, at: u32, children: Vec<Child>) -> Node {
    let mut span = Span::at(at);
    let mut first = true;
    for c in &children {
        let s = match c {
            Child::Node(n) => n.span,
            Child::Token(t) => t.span,
        };
        span = if first { s } else { span.cover(s) };
        first = false;
    }
    Node { kind, span, children }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_span_covers_its_children() {
        let mut b = TreeBuilder::new();
        b.start_node(NodeKind::KeyValue, 0);
        b.token(Token { kind: TokenKind::Ident, span: Span::new(4, 7) });
        b.token(Token { kind: TokenKind::Eq, span: Span::new(8, 9) });
        b.finish_node();
        let root = b.finish();
        assert_eq!(root.nodes().next().unwrap().span, Span::new(4, 9));
    }

    #[test]
    fn start_node_at_rewraps_existing_children() {
        let mut b = TreeBuilder::new();
        let cp = b.checkpoint();
        b.token(Token { kind: TokenKind::Ident, span: Span::new(0, 3) });
        b.start_node_at(cp, NodeKind::WhenExpr);
        b.token(Token { kind: TokenKind::Ident, span: Span::new(4, 8) });
        b.finish_node();
        let root = b.finish();
        let wrapped = root.nodes().next().unwrap();
        assert_eq!(wrapped.kind, NodeKind::WhenExpr);
        assert_eq!(wrapped.span, Span::new(0, 8));
        assert_eq!(wrapped.tokens().count(), 2);
    }
}
