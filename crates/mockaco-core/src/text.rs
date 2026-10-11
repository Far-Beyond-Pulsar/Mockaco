//! Document text as a rope.
//!
//! `Text` is an immutable-by-value, cheaply cloned B-tree of string chunks.
//! Every operation the editor performs per keystroke or per frame (an edit, a
//! line lookup, reading the visible lines, converting a position) costs
//! O(log n) in the document's size, and cloning a version for a snapshot or
//! the undo history shares all unchanged chunks. Nothing here ever needs the
//! whole document as one contiguous string; callers that truly do (saving,
//! a full-text search, a language server's full sync) ask for it explicitly
//! with [`Text::to_string`] and pay for it there, not on every edit.
//!
//! All offsets are UTF-8 byte offsets, as everywhere else in Mockaco. Only LF
//! is a line break; a CR directly before it belongs to the line ending and is
//! excluded from [`Text::line_end`], and a lone CR is ordinary text.

use crate::position_map::{ColumnEncoding, LineColumn, PositionError, TextPosition};
use ropey::Rope;
use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

#[derive(Clone, Default)]
pub struct Text {
    rope: Rope,
}

impl Text {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// Like [`str::is_char_boundary`]: the start and end are boundaries,
    /// anything past the end is not.
    pub fn is_char_boundary(&self, byte: usize) -> bool {
        let len = self.len();
        if byte == 0 || byte == len {
            return true;
        }
        if byte > len {
            return false;
        }
        // Chunk edges always fall on char boundaries.
        let (chunk, chunk_start, _, _) = self.rope.chunk_at_byte(byte);
        chunk.is_char_boundary(byte - chunk_start)
    }

    /// The text in `range`, borrowed when it lies inside one chunk.
    ///
    /// # Panics
    ///
    /// Like `&str` indexing: when the range is out of bounds or either end
    /// is not a char boundary.
    pub fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        self.get(range.clone()).unwrap_or_else(|| {
            panic!(
                "byte range {range:?} is not valid in text of length {}",
                self.len()
            )
        })
    }

    /// [`Self::slice`] without panicking.
    pub fn get(&self, range: Range<usize>) -> Option<Cow<'_, str>> {
        if range.start > range.end
            || range.end > self.len()
            || !self.is_char_boundary(range.start)
            || !self.is_char_boundary(range.end)
        {
            return None;
        }
        let slice = self.rope.byte_slice(range);
        Some(match slice.as_str() {
            Some(text) => Cow::Borrowed(text),
            None => Cow::Owned(slice.to_string()),
        })
    }

    /// Every chunk, in order.
    pub fn chunks(&self) -> impl Iterator<Item = &str> + '_ {
        self.rope.chunks()
    }

    /// The chunks covering `range`, trimmed to it.
    pub fn chunks_in(&self, range: Range<usize>) -> impl Iterator<Item = &str> + '_ {
        self.rope.byte_slice(range).chunks()
    }

    /// The bytes of `range` as a sequence of slices, without requiring the
    /// range to fall on char boundaries (parsers address raw bytes).
    pub fn byte_chunks_in(&self, range: Range<usize>) -> ByteChunks<'_> {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        ByteChunks {
            text: self,
            at: start,
            end,
        }
    }

    /// The chunk containing `byte` and the byte offset it starts at.
    pub fn chunk_at(&self, byte: usize) -> (&str, usize) {
        let (chunk, start, _, _) = self.rope.chunk_at_byte(byte.min(self.len()));
        (chunk, start)
    }

    /// `(offset, char)` from `byte` forward, like `str::char_indices` on the
    /// rest of the text. `byte` must be a char boundary.
    pub fn chars_from(&self, byte: usize) -> CharIndices<'_> {
        CharIndices {
            chars: self.rope.chars_at(self.rope.byte_to_char(byte)),
            byte,
            reversed: false,
        }
    }

    /// `(offset, char)` from just before `byte` backward. `byte` must be a
    /// char boundary.
    pub fn chars_before(&self, byte: usize) -> CharIndices<'_> {
        CharIndices {
            chars: self.rope.chars_at(self.rope.byte_to_char(byte)).reversed(),
            byte,
            reversed: true,
        }
    }

    /// The char starting at `byte`.
    pub fn char_at(&self, byte: usize) -> Option<char> {
        if byte >= self.len() || !self.is_char_boundary(byte) {
            return None;
        }
        self.chars_from(byte).next().map(|(_, c)| c)
    }

    /// Whether `other` is this very rope (not merely equal text): an O(1)
    /// identity check, for caches keyed by a version of the text.
    pub fn is_same_instance(&self, other: &Text) -> bool {
        self.rope.is_instance(&other.rope)
    }

    /// Replaces `range` with `text`. O(log n + edit size).
    ///
    /// # Panics
    ///
    /// When the range is invalid (see [`Self::slice`]).
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        assert!(
            range.start <= range.end
                && range.end <= self.len()
                && self.is_char_boundary(range.start)
                && self.is_char_boundary(range.end),
            "byte range {range:?} is not valid in text of length {}",
            self.len()
        );
        let start = self.rope.byte_to_char(range.start);
        if range.start != range.end {
            let end = self.rope.byte_to_char(range.end);
            self.rope.remove(start..end);
        }
        if !text.is_empty() {
            self.rope.insert(start, text);
        }
    }

    // ---- lines -------------------------------------------------------------

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line_start(&self, line: usize) -> Result<usize, PositionError> {
        if line >= self.line_count() {
            return Err(PositionError::LineOutOfBounds(line));
        }
        Ok(self.rope.line_to_byte(line))
    }

    /// The end of `line`'s content: before its LF, and before a CR that
    /// directly precedes the LF (or ends the text).
    pub fn line_end(&self, line: usize) -> Result<usize, PositionError> {
        let start = self.line_start(line)?;
        let mut end = if line + 1 < self.line_count() {
            self.rope.line_to_byte(line + 1) - 1
        } else {
            self.len()
        };
        if end > start && self.rope.byte(end - 1) == b'\r' {
            end -= 1;
        }
        Ok(end)
    }

    /// `line_start(line)..line_end(line)`.
    pub fn line_range(&self, line: usize) -> Result<Range<usize>, PositionError> {
        Ok(self.line_start(line)?..self.line_end(line)?)
    }

    /// The content of `line`, without its line ending.
    pub fn line(&self, line: usize) -> Result<Cow<'_, str>, PositionError> {
        let range = self.line_range(line)?;
        Ok(self.slice(range))
    }

    /// Every line's content in order, without line endings. Streams the rope
    /// in O(1) amortized per line; for building whole-document indexes once.
    pub fn lines(&self) -> impl Iterator<Item = Cow<'_, str>> + '_ {
        self.rope.lines().map(|line| {
            let mut len = line.len_bytes();
            if len > 0 && line.byte(len - 1) == b'\n' {
                len -= 1;
            }
            if len > 0 && line.byte(len - 1) == b'\r' {
                len -= 1;
            }
            let content = line.byte_slice(..len);
            match content.as_str() {
                Some(text) => Cow::Borrowed(text),
                None => Cow::Owned(content.to_string()),
            }
        })
    }

    pub fn byte_to_line(&self, byte: usize) -> Result<usize, PositionError> {
        if byte > self.len() {
            return Err(PositionError::OutOfBounds {
                offset: byte,
                len: self.len(),
            });
        }
        Ok(self.rope.byte_to_line(byte))
    }

    pub fn byte_to_line_column(&self, byte: usize) -> Result<LineColumn, PositionError> {
        let line = self.byte_to_line(byte)?;
        let start = self.line_start(line)?;
        let end = self.line_end(line)?;
        Ok(LineColumn {
            line,
            column: byte.min(end) - start,
        })
    }

    pub fn line_column_to_byte(&self, position: LineColumn) -> Result<usize, PositionError> {
        let start = self.line_start(position.line)?;
        let end = self.line_end(position.line)?;
        let byte = start + position.column;
        if byte > end {
            return Err(PositionError::ColumnOutOfBounds {
                line: position.line,
                column: position.column,
                encoding: ColumnEncoding::Bytes,
            });
        }
        Ok(byte)
    }

    fn validate_byte(&self, byte: usize) -> Result<(), PositionError> {
        if byte > self.len() {
            return Err(PositionError::OutOfBounds {
                offset: byte,
                len: self.len(),
            });
        }
        if !self.is_char_boundary(byte) {
            return Err(PositionError::InvalidUtf8Boundary(byte));
        }
        Ok(())
    }

    /// UTF-16 code units before `byte`.
    pub fn byte_to_utf16(&self, byte: usize) -> Result<usize, PositionError> {
        self.validate_byte(byte)?;
        Ok(self.rope.char_to_utf16_cu(self.rope.byte_to_char(byte)))
    }

    pub fn byte_to_utf16_position(&self, byte: usize) -> Result<TextPosition, PositionError> {
        self.validate_byte(byte)?;
        let line = self.byte_to_line(byte)?;
        let start = self.line_start(line)?;
        let end = self.line_end(line)?.min(byte);
        Ok(TextPosition {
            line,
            column: self.byte_to_utf16(end)? - self.byte_to_utf16(start)?,
        })
    }

    pub fn utf16_to_byte(&self, utf16: usize) -> Result<usize, PositionError> {
        let total = self.rope.len_utf16_cu();
        if utf16 > total {
            return Err(PositionError::OutOfBounds {
                offset: utf16,
                len: total,
            });
        }
        let char_index = self.rope.utf16_cu_to_char(utf16);
        if self.rope.char_to_utf16_cu(char_index) != utf16 {
            // Inside a surrogate pair.
            let byte = self.rope.char_to_byte(char_index);
            return Err(PositionError::ColumnOutOfBounds {
                line: self.rope.byte_to_line(byte),
                column: utf16,
                encoding: ColumnEncoding::Utf16,
            });
        }
        Ok(self.rope.char_to_byte(char_index))
    }

    pub fn utf16_position_to_byte(&self, position: TextPosition) -> Result<usize, PositionError> {
        let start = self.line_start(position.line)?;
        let end = self.line_end(position.line)?;
        let out_of_bounds = || PositionError::ColumnOutOfBounds {
            line: position.line,
            column: position.column,
            encoding: ColumnEncoding::Utf16,
        };
        let line_start_units = self.byte_to_utf16(start)?;
        let byte = self
            .utf16_to_byte(line_start_units + position.column)
            .map_err(|_| out_of_bounds())?;
        if byte > end {
            return Err(out_of_bounds());
        }
        Ok(byte)
    }

    pub fn position_to_byte(
        &self,
        position: TextPosition,
        encoding: ColumnEncoding,
    ) -> Result<usize, PositionError> {
        match encoding {
            ColumnEncoding::Bytes => self.line_column_to_byte(LineColumn {
                line: position.line,
                column: position.column,
            }),
            ColumnEncoding::Utf16 => self.utf16_position_to_byte(position),
        }
    }
}

/// Char iterator with byte offsets; see [`Text::chars_from`] and
/// [`Text::chars_before`].
pub struct CharIndices<'a> {
    chars: ropey::iter::Chars<'a>,
    byte: usize,
    reversed: bool,
}

impl Iterator for CharIndices<'_> {
    type Item = (usize, char);

    fn next(&mut self) -> Option<Self::Item> {
        let character = self.chars.next()?;
        if self.reversed {
            self.byte -= character.len_utf8();
            Some((self.byte, character))
        } else {
            let at = self.byte;
            self.byte += character.len_utf8();
            Some((at, character))
        }
    }
}

/// See [`Text::byte_chunks_in`].
pub struct ByteChunks<'a> {
    text: &'a Text,
    at: usize,
    end: usize,
}

impl<'a> Iterator for ByteChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.end {
            return None;
        }
        let (chunk, start) = self.text.chunk_at(self.at);
        let bytes = chunk.as_bytes();
        let from = self.at - start;
        let to = bytes.len().min(self.end - start);
        self.at = start + to;
        Some(&bytes[from..to])
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self::new(&text)
    }
}

impl From<&String> for Text {
    fn from(text: &String) -> Self {
        Self::new(text)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.to_string(), f)
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.rope == other.rope
    }
}

impl Eq for Text {}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.rope == other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.rope == *other
    }
}

impl PartialEq<String> for Text {
    fn eq(&self, other: &String) -> bool {
        self.rope == other.as_str()
    }
}

impl PartialEq<Text> for str {
    fn eq(&self, other: &Text) -> bool {
        other == self
    }
}

impl PartialEq<Text> for &str {
    fn eq(&self, other: &Text) -> bool {
        other == *self
    }
}

impl PartialEq<Text> for String {
    fn eq(&self, other: &Text) -> bool {
        other == self
    }
}


impl From<&Text> for Text {
    fn from(text: &Text) -> Self {
        text.clone()
    }
}

impl PartialEq<str> for &Text {
    fn eq(&self, other: &str) -> bool {
        (*self).eq(other)
    }
}

impl PartialEq<String> for &Text {
    fn eq(&self, other: &String) -> bool {
        (*self).eq(other.as_str())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_match_the_lf_model() {
        let text = Text::new("a\r\nbc\n\nlast\r");
        assert_eq!(text.line_count(), 4);
        assert_eq!(text.line(0).unwrap(), "a");
        assert_eq!(text.line(1).unwrap(), "bc");
        assert_eq!(text.line(2).unwrap(), "");
        assert_eq!(text.line(3).unwrap(), "last");
        assert_eq!(text.byte_to_line(text.len()).unwrap(), 3);
        let trailing = Text::new("x\n");
        assert_eq!(trailing.line_count(), 2);
        assert_eq!(trailing.line_range(1).unwrap(), 2..2);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines, ["a", "bc", "", "last"]);
        // A lone CR is text, not a break.
        assert_eq!(Text::new("a\rb").line_count(), 1);
    }

    #[test]
    fn edits_slices_and_char_iteration() {
        let mut text = Text::new("héllo wörld");
        assert!(!text.is_char_boundary(2));
        text.replace(0..1, "J");
        assert_eq!(text, "Jéllo wörld");
        let end = text.len();
        text.replace(end..end, "!");
        assert_eq!(text.slice(7..text.len()), "wörld!");
        let back: String = text.chars_before(3).map(|(_, c)| c).collect();
        assert_eq!(back, "éJ");
        let forward: Vec<_> = text.chars_from(1).take(2).collect();
        assert_eq!(forward, [(1, 'é'), (3, 'l')]);
        assert_eq!(text.char_at(1), Some('é'));
        assert_eq!(text.char_at(2), None);
    }

    #[test]
    fn utf16_round_trips_through_surrogates() {
        let text = Text::new("a😀b\nc");
        assert_eq!(text.byte_to_utf16(5).unwrap(), 3);
        assert_eq!(text.utf16_to_byte(3).unwrap(), 5);
        assert!(text.utf16_to_byte(2).is_err());
        let position = text.byte_to_utf16_position(7).unwrap();
        assert_eq!((position.line, position.column), (1, 0));
        assert_eq!(text.utf16_position_to_byte(position).unwrap(), 7);
    }

    #[test]
    fn a_large_text_edits_in_place() {
        let big = "line of text\n".repeat(200_000);
        let mut text = Text::new(&big);
        let snapshot = text.clone();
        let at = text.line_start(100_000).unwrap();
        text.replace(at..at, "x");
        assert_eq!(text.line(100_000).unwrap(), "xline of text");
        assert_eq!(snapshot.line(100_000).unwrap(), "line of text");
        assert_eq!(text.line_count(), 200_001);
    }
}
