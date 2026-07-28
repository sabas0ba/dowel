//! 読み込んだソースファイルの集合と、バイトオフセット→行桁の変換。

use crate::span::Span;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// ファイルの識別子。正規化したパスのハッシュである。
///
/// 読み込み順に依存しないため、プロセスを跨いでも同じファイルは同じ識別子を持つ
/// （[ADR-0009](../../../docs/adr/0009-file-identity.md)）。スパンは `FileId` と
/// 対でのみ意味を持つため、ストアに格納した値を復元する際に識別子を
/// 振り直す必要がない。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileId(pub u64);

impl FileId {
    /// パスから識別子を求める。
    ///
    /// 正規化に失敗した場合は与えられたパスをそのまま使う。存在しないファイルを
    /// 指す合成のソースがこれに当たる。この場合、別の書き方で同じファイルを
    /// 指すと別の識別子になるが、正規化できない以上は同一性を判定できない。
    pub fn of(path: &Path) -> FileId {
        use std::hash::{Hash, Hasher};
        let normalized = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        normalized.hash(&mut h);
        FileId(h.finish())
    }
}

/// 1 始まりの行と桁。桁は UTF-8 バイトではなく文字数で数える。
/// エディタとの突き合わせを目的とするため、表示側の単位に合わせる。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

struct SourceFile {
    path: PathBuf,
    text: String,
    /// 各行の開始バイトオフセット。二分探索で行番号を引く。
    line_starts: Vec<u32>,
}

fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    starts
}

/// 読み込んだ全ファイル。診断の描画と来歴の表示が参照する。
///
/// 識別子が添字ではなくハッシュであるため、格納には連想配列を用いる。
#[derive(Default)]
pub struct SourceMap {
    files: BTreeMap<FileId, SourceFile>,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    pub fn add(&mut self, path: impl Into<PathBuf>, text: String) -> FileId {
        let path = path.into();
        let mut id = FileId::of(&path);
        // ハッシュの衝突。64bit では実用上起きないが、起きた場合に別の
        // ファイルを同一と扱うと診断と来歴が静かに誤る。別の識別子を割り当てて
        // 正しさを保つ。この1件はプロセスを跨いだ安定性を失う。
        while self.files.get(&id).is_some_and(|f| f.path != path) {
            crate::log_debug!("file id collision on {}; probing", path.display());
            id = FileId(id.0.wrapping_add(1));
        }
        let line_starts = line_starts(&text);
        self.files.insert(id, SourceFile { path, text, line_starts });
        id
    }

    /// ディスクから読む。
    ///
    /// 既に読んだパスなら中身を差し替え、同じ識別子を返す。スパンは
    /// `FileId` と対で意味を持つため、読み直しで識別子が変わると、
    /// 増分の再利用で残った値の来歴が別のファイルを指すことになる。
    pub fn load(&mut self, path: impl AsRef<Path>) -> std::io::Result<FileId> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        Ok(self.add(path, text))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// 識別子に対応するファイルを読んでいるか。
    pub fn contains(&self, file: FileId) -> bool {
        self.files.contains_key(&file)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// 読み込んだ全ファイルのパスと識別子。パス順。
    pub fn paths(&self) -> Vec<(PathBuf, FileId)> {
        let mut out: Vec<(PathBuf, FileId)> =
            self.files.iter().map(|(id, f)| (f.path.clone(), *id)).collect();
        out.sort();
        out
    }

    /// 未知の識別子には空の記述を返す。診断の描画中に落ちるより、
    /// 位置の分からない診断を出す方が失うものが少ない。
    fn file(&self, file: FileId) -> Option<&SourceFile> {
        self.files.get(&file)
    }

    pub fn path(&self, file: FileId) -> &Path {
        self.file(file).map(|f| f.path.as_path()).unwrap_or(Path::new("<unknown>"))
    }

    pub fn text(&self, file: FileId) -> &str {
        self.file(file).map(|f| f.text.as_str()).unwrap_or("")
    }

    pub fn slice(&self, file: FileId, span: Span) -> &str {
        let text = self.text(file);
        let range = span.range();
        // 診断の描画中の panic を避ける。誤りの報告そのものが失われるためである。
        text.get(range).unwrap_or("")
    }

    pub fn line_col(&self, file: FileId, offset: u32) -> LineCol {
        let Some(f) = self.file(file) else { return LineCol { line: 1, col: 1 } };
        let idx = match f.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = f.line_starts[idx] as usize;
        let upto = f.text.get(line_start..offset as usize).unwrap_or("");
        LineCol { line: idx as u32 + 1, col: upto.chars().count() as u32 + 1 }
    }

    /// `offset` を含む行の本文（改行を含まない）。
    pub fn line_text(&self, file: FileId, line: u32) -> &str {
        let Some(f) = self.file(file) else { return "" };
        let idx = (line - 1) as usize;
        let Some(&start) = f.line_starts.get(idx) else { return "" };
        let end = f.line_starts.get(idx + 1).map(|&e| e as usize - 1).unwrap_or(f.text.len());
        f.text.get(start as usize..end).unwrap_or("").trim_end_matches('\r')
    }

    /// `path:line:col` 形式。来歴とログの表示に使う。
    pub fn location(&self, file: FileId, span: Span) -> String {
        let lc = self.line_col(file, span.start);
        format!("{}:{}:{}", self.path(file).display(), lc.line, lc.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_is_one_based_and_counts_chars() {
        let mut sm = SourceMap::new();
        // 非 ASCII は検査対象そのもの。桁をバイト数ではなく文字数で数えることの検査。
        let f = sm.add("t.build", "abc\nあいう\nxyz".to_string());
        assert_eq!(sm.line_col(f, 0), LineCol { line: 1, col: 1 });
        assert_eq!(sm.line_col(f, 3), LineCol { line: 1, col: 4 });
        assert_eq!(sm.line_col(f, 4), LineCol { line: 2, col: 1 });
        // 「あ」は 3 バイト。桁は文字数で数えるため 2 になる。
        assert_eq!(sm.line_col(f, 7), LineCol { line: 2, col: 2 });
    }

    #[test]
    fn reloading_a_path_keeps_its_identifier() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sourcemap-reload.build");
        std::fs::write(&path, "one\n").unwrap();

        let mut sm = SourceMap::new();
        let first = sm.load(&path).unwrap();
        assert_eq!(sm.text(first), "one\n");

        // 読み直しても識別子は同じ。中身と行頭表は差し替わる。
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let again = sm.load(&path).unwrap();
        assert_eq!(again, first);
        assert_eq!(sm.len(), 1);
        assert_eq!(sm.text(first), "one\ntwo\n");
        assert_eq!(sm.line_text(first, 2), "two");
    }

    #[test]
    fn the_same_path_gets_the_same_identifier_in_a_fresh_map() {
        // プロセスを跨いだ安定性の代理。別の `SourceMap` は別プロセスに相当する。
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sourcemap-stable.build");
        std::fs::write(&path, "x\n").unwrap();

        let mut a = SourceMap::new();
        // 先に別のファイルを読ませ、読み込み順を変える。添字であればここでずれる。
        a.add("other.build", "y\n".to_string());
        let first = a.load(&path).unwrap();

        let mut b = SourceMap::new();
        let second = b.load(&path).unwrap();
        assert_eq!(first, second, "the identifier depends on the load order");
    }

    #[test]
    fn different_paths_get_different_identifiers() {
        let mut sm = SourceMap::new();
        let a = sm.add("a.build", "x".to_string());
        let b = sm.add("b.build", "y".to_string());
        assert_ne!(a, b);
        assert_eq!(sm.len(), 2);
        assert_eq!(sm.text(a), "x");
        assert_eq!(sm.text(b), "y");
    }

    #[test]
    fn an_unknown_identifier_does_not_panic() {
        // 診断の描画は誤りの報告そのものである。未知の識別子で落とさない。
        let sm = SourceMap::new();
        let unknown = FileId(12345);
        assert_eq!(sm.text(unknown), "");
        assert_eq!(sm.line_text(unknown, 1), "");
        assert_eq!(sm.line_col(unknown, 0), LineCol { line: 1, col: 1 });
        assert_eq!(sm.slice(unknown, Span::new(0, 3)), "");
        assert!(!sm.contains(unknown));
    }

    #[test]
    fn line_text_excludes_the_newline() {
        let mut sm = SourceMap::new();
        let f = sm.add("t.build", "one\r\ntwo\nthree".to_string());
        assert_eq!(sm.line_text(f, 1), "one");
        assert_eq!(sm.line_text(f, 2), "two");
        assert_eq!(sm.line_text(f, 3), "three");
        assert_eq!(sm.line_text(f, 4), "");
    }
}
