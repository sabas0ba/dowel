//! 位置つき診断。
//!
//! docs/30-devexp.md 4節の方針により、診断は人間向けの描画と
//! 機械可読な JSON の双方を持つ。修正提案は span と置換文字列の対で表現し、
//! エージェントが機械的に適用できる形にする。

use crate::json::JsonWriter;
use crate::source::SourceMap;
use crate::span::Span;
use crate::FileId;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Severity::Error => "\x1b[1;31m",
            Severity::Warning => "\x1b[1;33m",
            Severity::Note => "\x1b[1;36m",
        }
    }
}

/// ソース上の指し示し。`primary` は診断の主因、それ以外は補助。
#[derive(Clone, Debug)]
pub struct Label {
    pub file: FileId,
    pub span: Span,
    pub message: String,
    pub primary: bool,
}

impl Label {
    pub fn primary(file: FileId, span: Span, message: impl Into<String>) -> Label {
        Label { file, span, message: message.into(), primary: true }
    }

    pub fn secondary(file: FileId, span: Span, message: impl Into<String>) -> Label {
        Label { file, span, message: message.into(), primary: false }
    }
}

/// 機械適用可能な修正提案。`span` を `replacement` で置き換える。
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub file: FileId,
    pub span: Span,
    pub replacement: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    /// 安定した識別子。`unknown-property` のようなケバブケース。
    /// 数値コードを採らないのは、追加時に番号の払い出しが要らないため。
    pub code: &'static str,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    pub fn warning(code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic { severity: Severity::Warning, ..Diagnostic::error(code, message) }
    }

    pub fn with_label(mut self, label: Label) -> Diagnostic {
        self.labels.push(label);
        self
    }

    pub fn at(self, file: FileId, span: Span, message: impl Into<String>) -> Diagnostic {
        self.with_label(Label::primary(file, span, message))
    }

    pub fn note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    pub fn suggest(
        mut self,
        file: FileId,
        span: Span,
        replacement: impl Into<String>,
        message: impl Into<String>,
    ) -> Diagnostic {
        self.suggestions.push(Suggestion {
            file,
            span,
            replacement: replacement.into(),
            message: message.into(),
        });
        self
    }

    pub fn primary_label(&self) -> Option<&Label> {
        self.labels.iter().find(|l| l.primary).or_else(|| self.labels.first())
    }
}

/// 診断の集積。評価は最初の誤りで停止しない（docs/20-architecture.md 2節）ため、
/// 収集して最後にまとめて描画する。
#[derive(Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Diagnostics {
        Diagnostics::default()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(other);
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    pub fn take(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.items)
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Error).count()
    }

    pub fn warning_count(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Warning).count()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

/// 人間向けの描画。rustc の書式に倣う。既知の書式に合わせることで
/// 利用者と LLM の双方が読み方を再学習しなくて済む。
pub fn render(d: &Diagnostic, sm: &SourceMap, color: bool) -> String {
    let (bold, reset, blue) =
        if color { ("\x1b[1m", "\x1b[0m", "\x1b[1;34m") } else { ("", "", "") };
    let sev_color = if color { d.severity.color() } else { "" };
    let mut out = String::new();

    out.push_str(&format!(
        "{sev_color}{}[{}]{reset}{bold}: {}{reset}\n",
        d.severity.label(),
        d.code,
        d.message
    ));

    let Some(primary) = d.primary_label() else {
        for n in &d.notes {
            out.push_str(&format!("  = note: {n}\n"));
        }
        return out;
    };

    let lc = sm.line_col(primary.file, primary.span.start);
    let gutter = lc.line.to_string().len().max(1);
    let pad = " ".repeat(gutter);

    out.push_str(&format!(
        "{pad}{blue}-->{reset} {}:{}:{}\n",
        sm.path(primary.file).display(),
        lc.line,
        lc.col
    ));

    for label in d.labels.iter().filter(|l| l.file == primary.file) {
        render_label(&mut out, label, sm, gutter, color);
    }

    for n in &d.notes {
        out.push_str(&format!("{pad}  = note: {n}\n"));
    }
    for s in &d.suggestions {
        out.push_str(&format!("{pad}  = help: {} — `{}`\n", s.message, s.replacement));
    }
    out
}

fn render_label(out: &mut String, label: &Label, sm: &SourceMap, gutter: usize, color: bool) {
    let (reset, blue) = if color { ("\x1b[0m", "\x1b[1;34m") } else { ("", "") };
    let marker_color = if color {
        if label.primary {
            "\x1b[1;31m"
        } else {
            "\x1b[1;34m"
        }
    } else {
        ""
    };
    let lc = sm.line_col(label.file, label.span.start);
    let text = sm.line_text(label.file, lc.line);
    let pad = " ".repeat(gutter);

    out.push_str(&format!("{pad} {blue}|{reset}\n"));
    out.push_str(&format!("{:>gutter$} {blue}|{reset} {}\n", lc.line, text, gutter = gutter));

    // キャレットは文字数単位で置く。タブは1文字として扱い、桁ずれは許容する。
    let end_lc = sm.line_col(label.file, label.span.end);
    let width = if end_lc.line == lc.line {
        (end_lc.col.saturating_sub(lc.col)).max(1)
    } else {
        (text.chars().count() as u32 + 1).saturating_sub(lc.col).max(1)
    };
    let marker = if label.primary { "^" } else { "-" };
    out.push_str(&format!(
        "{pad} {blue}|{reset} {}{marker_color}{}{reset} {marker_color}{}{reset}\n",
        " ".repeat(lc.col as usize - 1),
        marker.repeat(width as usize),
        label.message
    ));
}

/// 機械可読な診断。`--message-format=json` の実体。
/// 1診断1行の JSON とし、逐次消費できるようにする。
pub fn render_json(d: &Diagnostic, sm: &SourceMap) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.field_str("severity", d.severity.label());
    w.field_str("code", d.code);
    w.field_str("message", &d.message);

    w.key("labels").begin_array();
    for l in &d.labels {
        let lc = sm.line_col(l.file, l.span.start);
        let end = sm.line_col(l.file, l.span.end);
        w.begin_object();
        w.field_bool("primary", l.primary);
        w.field_str("file", &sm.path(l.file).display().to_string());
        w.field_u64("byte_start", l.span.start as u64);
        w.field_u64("byte_end", l.span.end as u64);
        w.field_u64("line", lc.line as u64);
        w.field_u64("column", lc.col as u64);
        w.field_u64("line_end", end.line as u64);
        w.field_u64("column_end", end.col as u64);
        w.field_str("message", &l.message);
        w.end_object();
    }
    w.end_array();

    w.field_strs("notes", d.notes.iter().map(|s| s.as_str()));

    w.key("suggestions").begin_array();
    for s in &d.suggestions {
        let lc = sm.line_col(s.file, s.span.start);
        w.begin_object();
        w.field_str("file", &sm.path(s.file).display().to_string());
        w.field_u64("byte_start", s.span.start as u64);
        w.field_u64("byte_end", s.span.end as u64);
        w.field_u64("line", lc.line as u64);
        w.field_u64("column", lc.col as u64);
        w.field_str("replacement", &s.replacement);
        w.field_str("message", &s.message);
        w.end_object();
    }
    w.end_array();

    w.end_object();
    w.finish()
}

/// 似た名前の候補を返す。未知のプロパティ名に対する修正提案に使う。
/// 編集距離は Levenshtein。候補が多くないため素朴な実装で足りる。
pub fn closest<'a>(needle: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    // 閾値は文字数で決める。バイト長で決めると非 ASCII の語が
    // 無関係な候補に一致してしまう。
    let max = (needle.chars().count() / 3).max(1) + 1;
    candidates
        .into_iter()
        .map(|c| (edit_distance(needle, c), c))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (SourceMap, FileId) {
        let mut sm = SourceMap::new();
        let f = sm.add(
            "libfoo/dowel.build",
            "[lib.foo.public]\n  include = [dir(\"include\")]\n".to_string(),
        );
        (sm, f)
    }

    #[test]
    fn human_rendering_shows_caret_and_location() {
        let (sm, f) = sample();
        let d = Diagnostic::error("unknown-property", "unknown property `include`")
            .at(f, Span::new(19, 26), "`lib.public` has no property with this name")
            .suggest(f, Span::new(19, 26), "includes", "did you mean `includes`?");
        let out = render(&d, &sm, false);
        assert!(out.contains("error[unknown-property]"), "{out}");
        assert!(out.contains("--> libfoo/dowel.build:2:3"), "{out}");
        assert!(out.contains("^^^^^^^"), "{out}");
        assert!(out.contains("= help:"), "{out}");
    }

    #[test]
    fn json_rendering_carries_location_and_replacement() {
        let (sm, f) = sample();
        let d = Diagnostic::error("unknown-property", "unknown property")
            .at(f, Span::new(19, 26), "here")
            .suggest(f, Span::new(19, 26), "includes", "fix the spelling");
        let json = render_json(&d, &sm);
        assert!(json.contains(r#""severity":"error""#), "{json}");
        assert!(json.contains(r#""line":2"#), "{json}");
        assert!(json.contains(r#""column":3"#), "{json}");
        assert!(json.contains(r#""replacement":"includes""#), "{json}");
    }

    #[test]
    fn closest_finds_typos_but_not_distant_words() {
        let cands = ["includes", "defines", "flags", "deps"];
        assert_eq!(closest("include", cands), Some("includes"));
        assert_eq!(closest("define", cands), Some("defines"));
        assert_eq!(closest("totally_unrelated", cands), None);
        // 非 ASCII は検査対象そのもの。閾値をバイト長で決めると無関係な候補に一致する。
        assert_eq!(closest("完全に別物", cands), None);
    }

    #[test]
    fn collector_counts_errors_and_warnings() {
        let (_, f) = sample();
        let mut ds = Diagnostics::new();
        ds.push(Diagnostic::warning("w", "warn").at(f, Span::EMPTY, ""));
        assert!(!ds.has_errors());
        ds.push(Diagnostic::error("e", "err").at(f, Span::EMPTY, ""));
        assert!(ds.has_errors());
        assert_eq!(ds.error_count(), 1);
        assert_eq!(ds.warning_count(), 1);
    }
}
