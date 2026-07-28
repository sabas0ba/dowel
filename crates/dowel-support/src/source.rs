//! 読み込んだソースファイルの集合と、バイトオフセット→行桁の変換。

use crate::span::Span;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `SourceMap` 内でのファイルの識別子。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileId(pub u32);

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
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
    /// パス → 識別子。同じファイルを読み直しても識別子を変えないため。
    by_path: BTreeMap<PathBuf, FileId>,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    pub fn add(&mut self, path: impl Into<PathBuf>, text: String) -> FileId {
        let id = FileId(self.files.len() as u32);
        let path = path.into();
        let line_starts = line_starts(&text);
        self.by_path.insert(path.clone(), id);
        self.files.push(SourceFile { path, text, line_starts });
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
        match self.by_path.get(path).copied() {
            Some(id) => {
                let f = &mut self.files[id.0 as usize];
                f.line_starts = line_starts(&text);
                f.text = text;
                Ok(id)
            }
            None => Ok(self.add(path, text)),
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// 読み込んだ全ファイルの識別子とパス。
    pub fn paths(&self) -> Vec<(PathBuf, FileId)> {
        self.files.iter().enumerate().map(|(i, f)| (f.path.clone(), FileId(i as u32))).collect()
    }

    pub fn path(&self, file: FileId) -> &Path {
        &self.files[file.0 as usize].path
    }

    pub fn text(&self, file: FileId) -> &str {
        &self.files[file.0 as usize].text
    }

    pub fn slice(&self, file: FileId, span: Span) -> &str {
        let text = self.text(file);
        let range = span.range();
        // 診断の描画中の panic を避ける。誤りの報告そのものが失われるためである。
        text.get(range).unwrap_or("")
    }

    pub fn line_col(&self, file: FileId, offset: u32) -> LineCol {
        let f = &self.files[file.0 as usize];
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
        let f = &self.files[file.0 as usize];
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
    fn line_text_excludes_the_newline() {
        let mut sm = SourceMap::new();
        let f = sm.add("t.build", "one\r\ntwo\nthree".to_string());
        assert_eq!(sm.line_text(f, 1), "one");
        assert_eq!(sm.line_text(f, 2), "two");
        assert_eq!(sm.line_text(f, 3), "three");
        assert_eq!(sm.line_text(f, 4), "");
    }
}
