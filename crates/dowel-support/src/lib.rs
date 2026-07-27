//! 全クレートが共有する基盤。
//!
//! ここに置くものの基準は「診断・来歴・ログのいずれかに関わり、かつ
//! 構文にも評価にも依存しないもの」である。外部 crate に依存しない
//! （[ADR-0007](../../../docs/adr/0007-implementation-language.md)）。

pub mod diag;
pub mod json;
pub mod log;
pub mod source;
pub mod span;

pub use diag::{Diagnostic, Label, Severity, Suggestion};
pub use source::{FileId, SourceMap};
pub use span::Span;
