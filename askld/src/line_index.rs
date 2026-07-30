//! Byte-offset ↔ line/column conversion for source-file content.
//!
//! Line boundaries are LF (`\n`); a `\r` is treated as an ordinary byte, so on
//! CRLF files positions are LF-based and a column may include the trailing
//! `\r` of the preceding line (callers that care, e.g. `loc`, warn separately).
//!
//! Two directions are provided:
//! - [`line_to_offset`] / [`next_line_offset`]: 1-based line -> byte offset,
//!   used by the `loc` selector.
//! - [`LineIndex`]: byte offset -> 1-based line/column, used to render
//!   `file:line` locations in query results. Build the index once per file,
//!   then resolve many offsets in `O(log lines)` each.

/// A precomputed table of line-start byte offsets for one file's content.
///
/// A query result typically touches many symbols in the same file; building
/// this once and binary-searching each offset avoids re-scanning the whole
/// file per symbol.
pub struct LineIndex {
    /// `line_starts[i]` is the byte offset at which line `i + 1` begins.
    /// Always starts with `0`, so it is never empty.
    line_starts: Vec<usize>,
}

impl LineIndex {
    /// Build a line index by scanning `content` once for `\n` bytes.
    pub fn new(content: &[u8]) -> Self {
        let mut line_starts = vec![0usize];
        for (idx, byte) in content.iter().enumerate() {
            if *byte == b'\n' {
                line_starts.push(idx + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// The 1-based line number containing byte `offset`.
    ///
    /// An `offset` at or past end-of-content resolves to the last line, so
    /// out-of-range offsets never panic.
    pub fn line_of(&self, offset: usize) -> usize {
        self.line_starts.partition_point(|&start| start <= offset)
    }

    /// The 1-based `(line, column)` of byte `offset`. Column is byte-based
    /// within the line (column `1` is the first byte of the line).
    pub fn line_col_of(&self, offset: usize) -> (usize, usize) {
        let line = self.line_of(offset);
        let line_start = self.line_starts[line - 1];
        (line, offset - line_start + 1)
    }

    /// Number of lines in the indexed content (always at least 1).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }
}

/// Convert a 1-based line number to a byte offset (start of line).
pub fn line_to_offset(content: &[u8], line: usize) -> Option<i64> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }

    let mut current_line = 1usize;
    for (idx, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            current_line += 1;
            if current_line == line {
                return Some((idx + 1) as i64);
            }
        }
    }
    None
}

/// Find the byte offset of the next newline after `start`, or end of content.
pub fn next_line_offset(content: &[u8], start: i64) -> i64 {
    let start = start as usize;
    for idx in start..content.len() {
        if content[idx] == b'\n' {
            return idx as i64;
        }
    }
    content.len() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_to_offset_line_1() {
        assert_eq!(line_to_offset(b"hello\nworld\n", 1), Some(0));
    }

    #[test]
    fn line_to_offset_line_2() {
        assert_eq!(line_to_offset(b"hello\nworld\n", 2), Some(6));
    }

    #[test]
    fn line_to_offset_line_3() {
        assert_eq!(line_to_offset(b"hello\nworld\n", 3), Some(12));
    }

    #[test]
    fn line_to_offset_line_0_returns_none() {
        assert_eq!(line_to_offset(b"hello\n", 0), None);
    }

    #[test]
    fn line_to_offset_past_end_returns_none() {
        assert_eq!(line_to_offset(b"hello\n", 3), None);
    }

    #[test]
    fn line_to_offset_empty_content() {
        assert_eq!(line_to_offset(b"", 1), Some(0));
        assert_eq!(line_to_offset(b"", 2), None);
    }

    #[test]
    fn next_line_offset_finds_newline() {
        assert_eq!(next_line_offset(b"hello\nworld\n", 0), 5);
    }

    #[test]
    fn next_line_offset_at_end_of_content() {
        assert_eq!(next_line_offset(b"hello", 0), 5);
    }

    #[test]
    fn next_line_offset_from_mid_line() {
        assert_eq!(next_line_offset(b"hello\nworld\n", 6), 11);
    }

    #[test]
    fn line_of_maps_offsets_to_lines() {
        let li = LineIndex::new(b"hello\nworld\n");
        assert_eq!(li.line_of(0), 1); // 'h'
        assert_eq!(li.line_of(4), 1); // 'o'
        assert_eq!(li.line_of(5), 1); // '\n' terminates line 1
        assert_eq!(li.line_of(6), 2); // 'w'
        assert_eq!(li.line_of(11), 2); // '\n'
        assert_eq!(li.line_of(12), 3); // past the trailing newline
    }

    #[test]
    fn line_of_past_end_clamps_to_last_line() {
        let li = LineIndex::new(b"abc");
        assert_eq!(li.line_count(), 1);
        assert_eq!(li.line_of(100), 1);
    }

    #[test]
    fn line_of_empty_content() {
        let li = LineIndex::new(b"");
        assert_eq!(li.line_count(), 1);
        assert_eq!(li.line_of(0), 1);
    }

    #[test]
    fn line_col_of_computes_column() {
        let li = LineIndex::new(b"hello\nworld\n");
        assert_eq!(li.line_col_of(0), (1, 1));
        assert_eq!(li.line_col_of(4), (1, 5)); // 'o' on line 1
        assert_eq!(li.line_col_of(6), (2, 1)); // 'w' starts line 2
        assert_eq!(li.line_col_of(8), (2, 3)); // 'r' in "world"
    }

    #[test]
    fn line_index_agrees_with_line_to_offset() {
        let content = b"aa\nbbb\n\ncc\n";
        let li = LineIndex::new(content);
        for line in 1..=li.line_count() {
            if let Some(off) = line_to_offset(content, line) {
                assert_eq!(li.line_of(off as usize), line);
            }
        }
    }
}
