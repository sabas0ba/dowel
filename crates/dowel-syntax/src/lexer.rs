//! 字句解析。
//!
//! 入力の全バイトがちょうど1つのトークンに属する。空白・改行・コメントも
//! トークンとして返す。ロスレス CST（docs/20-architecture.md 2節）の前提であり、
//! ここで捨てたものは後から復元できない。

use dowel_support::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    // 些末部（trivia）。構文木には残すが、文法規則は読み飛ばす。
    Whitespace,
    Newline,
    Comment,

    Ident,
    Int,
    /// 引用符つき文字列。基本文字列 `"..."`、リテラル文字列 `'...'`、
    /// 複数行文字列 `"""..."""` を区別せず1種として扱い、
    /// 解釈は評価側（`dowel-eval`）が行う。
    Str,

    LBracket,
    RBracket,
    LBrace,
    RBrace,
    LParen,
    RParen,
    Comma,
    Eq,
    EqEq,
    Dot,
    FatArrow,

    /// 未知の文字。誤り耐性のため字句解析では停止しない。
    Unknown,
    Eof,
}

impl TokenKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, TokenKind::Whitespace | TokenKind::Newline | TokenKind::Comment)
    }

    /// 診断に出す名前。
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Whitespace => "whitespace",
            TokenKind::Newline => "a newline",
            TokenKind::Comment => "a comment",
            TokenKind::Ident => "an identifier",
            TokenKind::Int => "an integer",
            TokenKind::Str => "a string",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::Comma => "`,`",
            TokenKind::Eq => "`=`",
            TokenKind::EqEq => "`==`",
            TokenKind::Dot => "`.`",
            TokenKind::FatArrow => "`=>`",
            TokenKind::Unknown => "an unrecognized character",
            TokenKind::Eof => "end of input",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// 字句解析中に検出した誤り。パーサが診断へ変換する。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub span: Span,
    pub kind: LexErrorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexErrorKind {
    UnterminatedString,
    UnterminatedBlockComment,
    UnknownChar,
}

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexError>,
}

pub fn lex(src: &str) -> Lexed {
    Lexer { src: src.as_bytes(), text: src, pos: 0, tokens: Vec::new(), errors: Vec::new() }.run()
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
}

impl<'a> Lexer<'a> {
    fn run(mut self) -> Lexed {
        // 先頭の UTF-8 BOM は些末部として読み飛ばす。Windows のメモ帳や
        // PowerShell のリダイレクトが黙って付けるものであり、利用者が書いた
        // 覚えのない違いである。トークンとして残すのはロスレス性のため。
        // 先頭にしか意味を持たないので、途中の同じバイト列は今までどおり
        // 未知の文字として扱う。
        if self.src.starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.pos = 3;
            self.push(TokenKind::Whitespace, 0);
        }
        while self.pos < self.src.len() {
            self.token();
        }
        let end = self.src.len() as u32;
        self.tokens.push(Token { kind: TokenKind::Eof, span: Span::new(end, end) });
        Lexed { tokens: self.tokens, errors: self.errors }
    }

    fn peek(&self) -> u8 {
        self.src.get(self.pos).copied().unwrap_or(0)
    }

    fn peek_at(&self, n: usize) -> u8 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token { kind, span: Span::new(start as u32, self.pos as u32) });
    }

    fn error(&mut self, kind: LexErrorKind, start: usize) {
        self.errors.push(LexError { span: Span::new(start as u32, self.pos as u32), kind });
    }

    fn token(&mut self) {
        let start = self.pos;
        let c = self.peek();
        match c {
            b'\n' => {
                self.pos += 1;
                self.push(TokenKind::Newline, start);
            }
            b'\r' if self.peek_at(1) == b'\n' => {
                self.pos += 2;
                self.push(TokenKind::Newline, start);
            }
            b' ' | b'\t' | b'\r' => {
                while matches!(self.peek(), b' ' | b'\t' | b'\r') {
                    self.pos += 1;
                }
                self.push(TokenKind::Whitespace, start);
            }
            b'#' => {
                self.line_comment(start);
            }
            b'/' if self.peek_at(1) == b'/' => {
                self.line_comment(start);
            }
            b'/' if self.peek_at(1) == b'*' => {
                self.block_comment(start);
            }
            b'[' => self.punct(TokenKind::LBracket, 1, start),
            b']' => self.punct(TokenKind::RBracket, 1, start),
            b'{' => self.punct(TokenKind::LBrace, 1, start),
            b'}' => self.punct(TokenKind::RBrace, 1, start),
            b'(' => self.punct(TokenKind::LParen, 1, start),
            b')' => self.punct(TokenKind::RParen, 1, start),
            b',' => self.punct(TokenKind::Comma, 1, start),
            b'.' => self.punct(TokenKind::Dot, 1, start),
            b'=' if self.peek_at(1) == b'>' => self.punct(TokenKind::FatArrow, 2, start),
            b'=' if self.peek_at(1) == b'=' => self.punct(TokenKind::EqEq, 2, start),
            b'=' => self.punct(TokenKind::Eq, 1, start),
            b'"' | b'\'' => self.string(start),
            b'0'..=b'9' => self.number(start),
            b'-' | b'+' if self.peek_at(1).is_ascii_digit() => self.number(start),
            _ if is_ident_start(c) => {
                while is_ident_continue(self.peek()) {
                    self.pos += 1;
                }
                self.push(TokenKind::Ident, start);
            }
            _ => {
                // 未知の文字は1文字ずつではなく、次の認識可能な位置まで束ねる。
                // 診断の件数が入力の乱れの長さに比例して増えるのを避けるため。
                let ch_len = utf8_len(self.text, self.pos);
                self.pos += ch_len;
                while self.pos < self.src.len() && !self.is_recognizable(self.peek()) {
                    self.pos += utf8_len(self.text, self.pos);
                }
                self.push(TokenKind::Unknown, start);
                self.error(LexErrorKind::UnknownChar, start);
            }
        }
    }

    fn is_recognizable(&self, c: u8) -> bool {
        matches!(
            c,
            b'\n'
                | b'\r'
                | b' '
                | b'\t'
                | b'#'
                | b'/'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'('
                | b')'
                | b','
                | b'.'
                | b'='
                | b'"'
                | b'\''
        ) || c.is_ascii_digit()
            || is_ident_start(c)
    }

    fn punct(&mut self, kind: TokenKind, len: usize, start: usize) {
        self.pos += len;
        self.push(kind, start);
    }

    fn line_comment(&mut self, start: usize) {
        while self.pos < self.src.len() && self.peek() != b'\n' {
            self.pos += 1;
        }
        // 行末の `\r` はコメントに含めない。改行トークンの一部として扱う。
        if self.pos > start && self.src[self.pos - 1] == b'\r' {
            self.pos -= 1;
        }
        self.push(TokenKind::Comment, start);
    }

    fn block_comment(&mut self, start: usize) {
        self.pos += 2;
        let mut depth = 1usize;
        while self.pos < self.src.len() {
            if self.peek() == b'/' && self.peek_at(1) == b'*' {
                depth += 1;
                self.pos += 2;
            } else if self.peek() == b'*' && self.peek_at(1) == b'/' {
                depth -= 1;
                self.pos += 2;
                if depth == 0 {
                    break;
                }
            } else {
                self.pos += 1;
            }
        }
        if depth != 0 {
            self.error(LexErrorKind::UnterminatedBlockComment, start);
        }
        self.push(TokenKind::Comment, start);
    }

    fn string(&mut self, start: usize) {
        let quote = self.peek();
        // 複数行文字列は同じ引用符が3つ続く形。
        let triple = self.peek_at(1) == quote && self.peek_at(2) == quote;
        if triple {
            self.pos += 3;
            loop {
                if self.pos >= self.src.len() {
                    self.error(LexErrorKind::UnterminatedString, start);
                    break;
                }
                if self.peek() == quote && self.peek_at(1) == quote && self.peek_at(2) == quote {
                    self.pos += 3;
                    break;
                }
                // リテラル文字列（`'`）ではエスケープを解釈しない。
                if quote == b'"' && self.peek() == b'\\' && self.pos + 1 < self.src.len() {
                    self.pos += 1;
                }
                self.pos += 1;
            }
        } else {
            self.pos += 1;
            loop {
                if self.pos >= self.src.len() || self.peek() == b'\n' {
                    self.error(LexErrorKind::UnterminatedString, start);
                    break;
                }
                if self.peek() == quote {
                    self.pos += 1;
                    break;
                }
                if quote == b'"' && self.peek() == b'\\' && self.pos + 1 < self.src.len() {
                    self.pos += 1;
                }
                self.pos += 1;
            }
        }
        self.push(TokenKind::Str, start);
    }

    fn number(&mut self, start: usize) {
        if matches!(self.peek(), b'-' | b'+') {
            self.pos += 1;
        }
        // `_` による桁区切りは TOML に倣って許す。0x/0o/0b 接頭辞も同様。
        if self.peek() == b'0' && matches!(self.peek_at(1), b'x' | b'X' | b'o' | b'b') {
            self.pos += 2;
        }
        while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
            self.pos += 1;
        }
        self.push(TokenKind::Int, start);
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// `pos` から始まる UTF-8 文字のバイト長。不正なバイト列でも1以上を返す。
fn utf8_len(text: &str, pos: usize) -> usize {
    let b = text.as_bytes()[pos];
    let len = if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    };
    len.min(text.len() - pos).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).tokens.iter().map(|t| t.kind).filter(|k| !k.is_trivia()).collect()
    }

    /// ロスレス性の中心的な不変条件。全トークンのテキストを連結すると
    /// 元のソースに一致する。ここが崩れると CST が正本たりえない。
    fn assert_lossless(src: &str) {
        let lexed = lex(src);
        let mut out = String::new();
        for t in &lexed.tokens {
            out.push_str(&src[t.span.range()]);
        }
        assert_eq!(out, src, "the lexer cannot reproduce its input");
    }

    #[test]
    fn basic_token_sequence() {
        assert_eq!(
            kinds("[lib.foo]\nsources = glob(\"src/**.c\")\n"),
            vec![
                TokenKind::LBracket,
                TokenKind::Ident,
                TokenKind::Dot,
                TokenKind::Ident,
                TokenKind::RBracket,
                TokenKind::Ident,
                TokenKind::Eq,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::Str,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn recognizes_all_three_comment_forms() {
        assert_eq!(kinds("# a\n// b\n/* c */\n"), vec![TokenKind::Eof]);
        assert_lossless("# a\n// b\n/* c */\n");
    }

    #[test]
    fn nested_block_comments() {
        let lexed = lex("/* a /* b */ c */x");
        let non_trivia: Vec<_> =
            lexed.tokens.iter().filter(|t| !t.kind.is_trivia()).map(|t| t.kind).collect();
        assert_eq!(non_trivia, vec![TokenKind::Ident, TokenKind::Eof]);
        assert!(lexed.errors.is_empty());
    }

    #[test]
    fn unterminated_block_comment_reports_but_does_not_stop() {
        let lexed = lex("/* a");
        assert_eq!(lexed.errors.len(), 1);
        assert_eq!(lexed.errors[0].kind, LexErrorKind::UnterminatedBlockComment);
        assert_lossless("/* a");
    }

    #[test]
    fn all_three_string_forms() {
        assert_eq!(
            kinds(r#" "a" 'b' """c""" "#),
            vec![TokenKind::Str, TokenKind::Str, TokenKind::Str, TokenKind::Eof]
        );
    }

    #[test]
    fn escaped_quote_does_not_close_the_string() {
        let src = r#""a\"b" x"#;
        let lexed = lex(src);
        let first = lexed.tokens.iter().find(|t| t.kind == TokenKind::Str).unwrap();
        assert_eq!(&src[first.span.range()], r#""a\"b""#);
        assert!(lexed.errors.is_empty());
    }

    #[test]
    fn literal_strings_do_not_interpret_escapes() {
        let src = r"'a\' x";
        let lexed = lex(src);
        // `\` はリテラル文字列内で特別扱いされないため、`'a\'` で閉じる。
        let first = lexed.tokens.iter().find(|t| t.kind == TokenKind::Str).unwrap();
        assert_eq!(&src[first.span.range()], r"'a\'");
    }

    #[test]
    fn unterminated_string_stops_at_end_of_line() {
        let lexed = lex("a = \"unterminated\nb = 1\n");
        assert_eq!(lexed.errors.len(), 1);
        assert_eq!(lexed.errors[0].kind, LexErrorKind::UnterminatedString);
        // 次の行の解析は続行できる。
        assert!(lexed.tokens.iter().filter(|t| t.kind == TokenKind::Ident).count() >= 2);
    }

    #[test]
    fn distinguishes_arrow_from_equals() {
        assert_eq!(
            kinds("a => b == c = d"),
            vec![
                TokenKind::Ident,
                TokenKind::FatArrow,
                TokenKind::Ident,
                TokenKind::EqEq,
                TokenKind::Ident,
                TokenKind::Eq,
                TokenKind::Ident,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn signed_numbers_and_digit_separators() {
        assert_eq!(
            kinds("-1 +2 1_000 0xff"),
            vec![TokenKind::Int, TokenKind::Int, TokenKind::Int, TokenKind::Int, TokenKind::Eof]
        );
    }

    #[test]
    fn identifiers_may_contain_hyphens() {
        assert_eq!(kinds("winsock-shim"), vec![TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn unknown_characters_do_not_stop_the_lexer() {
        let src = "a = @@@ \n b = 1\n";
        let lexed = lex(src);
        assert_eq!(lexed.errors.len(), 1);
        assert_lossless(src);
    }

    #[test]
    fn non_ascii_input_is_reproduced() {
        // 非 ASCII は検査対象そのもの。多バイト文字を跨いでロスレス性が保たれるか。
        let src = "# 日本語のコメント\nname = \"あいう\"\n";
        assert_lossless(src);
        assert_eq!(
            kinds(src),
            vec![TokenKind::Ident, TokenKind::Eq, TokenKind::Str, TokenKind::Eof]
        );
    }

    #[test]
    fn crlf_is_a_single_newline() {
        let src = "a = 1\r\nb = 2\r\n";
        assert_lossless(src);
        let newlines = lex(src).tokens.iter().filter(|t| t.kind == TokenKind::Newline).count();
        assert_eq!(newlines, 2);
    }

    #[test]
    fn a_leading_bom_is_trivia() {
        // Windows のメモ帳や PowerShell のリダイレクトが黙って付ける。
        // 拒むと、利用者の画面には何も間違いが見えない（issue #34）。
        let src = "\u{feff}a = 1\n";
        let lexed = lex(src);
        assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
        assert_eq!(lexed.tokens[0].kind, TokenKind::Whitespace);
        assert_eq!(&src[lexed.tokens[0].span.range()], "\u{feff}");
        assert_lossless(src);
        assert_eq!(
            kinds(src),
            vec![TokenKind::Ident, TokenKind::Eq, TokenKind::Int, TokenKind::Eof]
        );
    }

    #[test]
    fn a_bom_elsewhere_is_still_unknown() {
        // BOM は先頭にしか意味を持たない。途中のものを黙って飲むと、
        // 見えない文字が本文に紛れたことを利用者へ伝えられない。
        let src = "a = 1\n\u{feff}b = 2\n";
        let lexed = lex(src);
        assert_eq!(lexed.errors.len(), 1);
        assert_eq!(lexed.errors[0].kind, LexErrorKind::UnknownChar);
        assert_lossless(src);
    }
}
