//! バイトオフセットによる範囲。
//!
//! 全ての値がソース位置を持つ（docs/20-architecture.md 2節「スパンの全面保持」）。
//! `Copy` に保てる大きさに留めるのは、値の構成要素として遍在するためである。

/// ファイル内のバイト範囲。`start <= end` を不変条件とする。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const EMPTY: Span = Span { start: 0, end: 0 };

    pub fn new(start: u32, end: u32) -> Span {
        debug_assert!(start <= end, "span start must not exceed end");
        Span { start, end }
    }

    pub fn at(offset: u32) -> Span {
        Span { start: offset, end: offset }
    }

    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// 双方を含む最小の範囲。部分木のスパンを子から畳み込むときに使う。
    pub fn cover(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    pub fn contains(&self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }

    pub fn range(&self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_spans_both_ranges() {
        assert_eq!(Span::new(3, 5).cover(Span::new(10, 12)), Span::new(3, 12));
        assert_eq!(Span::new(10, 12).cover(Span::new(3, 5)), Span::new(3, 12));
        assert_eq!(Span::new(3, 20).cover(Span::new(5, 6)), Span::new(3, 20));
    }

    #[test]
    fn contains_excludes_the_end() {
        let s = Span::new(2, 4);
        assert!(!s.contains(1));
        assert!(s.contains(2));
        assert!(s.contains(3));
        assert!(!s.contains(4));
    }
}
