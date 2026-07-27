//! `dowel.build` と `dowel.toml` の構文層。
//!
//! 両ファイルは同一の文法で解析する。`dowel.toml` を厳密な TOML に保つ制約は
//! 構文ではなく検証で課す（`dowel-eval` の `strict` 検査）。
//! [ADR-0003] は「パーサが2系統になる」と述べているが、狙いは
//! 「第三者ツールが `dowel.toml` を独自パーサなしで読めること」であり、
//! 式の出現を検証で拒否すればその保証は同じく得られる。木を1つに保つ分、
//! 来歴と診断の経路が単純になる。
//!
//! [ADR-0003]: ../../../docs/adr/0003-manifest-split.md

pub mod cst;
pub mod lexer;
pub mod parser;

pub use cst::{Child, Node, NodeKind};
pub use lexer::{Token, TokenKind};
pub use parser::{parse, Parsed};
