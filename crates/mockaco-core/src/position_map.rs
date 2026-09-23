use std::fmt;

pub type ByteOffset = usize;
pub type Utf16Offset = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnEncoding {
    Bytes,
    Utf16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineColumn {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextPosition {
    pub line: usize,
    pub column: usize,
}

pub type Position = TextPosition;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PositionError {
    OutOfBounds {
        offset: usize,
        len: usize,
    },
    InvalidUtf8Boundary(usize),
    LineOutOfBounds(usize),
    ColumnOutOfBounds {
        line: usize,
        column: usize,
        encoding: ColumnEncoding,
    },
}

impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { offset, len } => {
                write!(f, "offset {offset} is outside length {len}")
            }
            Self::InvalidUtf8Boundary(offset) => {
                write!(f, "offset {offset} is not a UTF-8 boundary")
            }
            Self::LineOutOfBounds(line) => write!(f, "line {line} is out of bounds"),
            Self::ColumnOutOfBounds {
                line,
                column,
                encoding,
            } => {
                write!(
                    f,
                    "column {column} is out of bounds on line {line} ({encoding:?})"
                )
            }
        }
    }
}

impl std::error::Error for PositionError {}

/// Newline-aware coordinate conversion for one immutable document version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionMap {
    text_len: usize,
    line_starts: Vec<usize>,
    line_ends: Vec<usize>,
}

impl PositionMap {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        let mut line_ends = Vec::new();
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                let mut end = index;
                if end > *line_starts.last().unwrap() && text.as_bytes()[end - 1] == b'\r' {
                    end -= 1;
                }
                line_ends.push(end);
                line_starts.push(index + 1);
            }
        }
        let final_end = text.len();
        if line_ends.len() < line_starts.len() {
            let start = *line_starts.last().unwrap();
            let mut end = final_end;
            if end > start && text.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            line_ends.push(end);
        }
        Self {
            text_len: text.len(),
            line_starts,
            line_ends,
        }
    }

    pub fn len_bytes(&self) -> usize {
        self.text_len
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn line_start(&self, line: usize) -> Result<usize, PositionError> {
        self.line_starts
            .get(line)
            .copied()
            .ok_or(PositionError::LineOutOfBounds(line))
    }

    pub fn line_end(&self, line: usize) -> Result<usize, PositionError> {
        self.line_ends
            .get(line)
            .copied()
            .ok_or(PositionError::LineOutOfBounds(line))
    }

    pub fn byte_to_line(&self, byte: usize) -> Result<usize, PositionError> {
        if byte > self.text_len {
            return Err(PositionError::OutOfBounds {
                offset: byte,
                len: self.text_len,
            });
        }
        match self.line_starts.binary_search(&byte) {
            Ok(line) => Ok(line),
            Err(index) => Ok(index.saturating_sub(1)),
        }
    }

    pub fn byte_to_line_column(&self, byte: usize) -> Result<LineColumn, PositionError> {
        let line = self.byte_to_line(byte)?;
        let start = self.line_start(line)?;
        let end = self.line_end(line)?;
        if byte > end {
            return Ok(LineColumn {
                line,
                column: end - start,
            });
        }
        Ok(LineColumn {
            line,
            column: byte - start,
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

    pub fn byte_to_utf16(&self, text: &str, byte: usize) -> Result<usize, PositionError> {
        self.validate_byte(text, byte)?;
        Ok(text[..byte].encode_utf16().count())
    }

    pub fn byte_to_utf16_position(
        &self,
        text: &str,
        byte: usize,
    ) -> Result<TextPosition, PositionError> {
        self.validate_byte(text, byte)?;
        let line = self.byte_to_line(byte)?;
        let start = self.line_start(line)?;
        let end = self.line_end(line)?.min(byte);
        let column = text[start..end].encode_utf16().count();
        Ok(TextPosition { line, column })
    }

    pub fn utf16_to_byte(&self, text: &str, utf16: usize) -> Result<usize, PositionError> {
        if utf16 == 0 {
            return Ok(0);
        }
        let mut units = 0;
        for (offset, character) in text.char_indices() {
            if units == utf16 {
                return Ok(offset);
            }
            units += character.len_utf16();
            if units > utf16 {
                let line = self.byte_to_line(offset)?;
                return Err(PositionError::ColumnOutOfBounds {
                    line,
                    column: utf16,
                    encoding: ColumnEncoding::Utf16,
                });
            }
        }
        if units == utf16 {
            Ok(text.len())
        } else {
            Err(PositionError::OutOfBounds {
                offset: utf16,
                len: units,
            })
        }
    }

    pub fn utf16_position_to_byte(
        &self,
        text: &str,
        position: TextPosition,
    ) -> Result<usize, PositionError> {
        let start = self.line_start(position.line)?;
        let end = self.line_end(position.line)?;
        let line = &text[start..end];
        let mut units = 0;
        for (offset, character) in line.char_indices() {
            if units == position.column {
                return Ok(start + offset);
            }
            units += character.len_utf16();
            if units > position.column {
                return Err(PositionError::ColumnOutOfBounds {
                    line: position.line,
                    column: position.column,
                    encoding: ColumnEncoding::Utf16,
                });
            }
        }
        if units == position.column {
            Ok(end)
        } else {
            Err(PositionError::ColumnOutOfBounds {
                line: position.line,
                column: position.column,
                encoding: ColumnEncoding::Utf16,
            })
        }
    }

    pub fn position_to_byte(
        &self,
        text: &str,
        position: TextPosition,
        encoding: ColumnEncoding,
    ) -> Result<usize, PositionError> {
        match encoding {
            ColumnEncoding::Bytes => self.line_column_to_byte(LineColumn {
                line: position.line,
                column: position.column,
            }),
            ColumnEncoding::Utf16 => self.utf16_position_to_byte(text, position),
        }
    }

    fn validate_byte(&self, text: &str, byte: usize) -> Result<(), PositionError> {
        if byte > self.text_len {
            return Err(PositionError::OutOfBounds {
                offset: byte,
                len: self.text_len,
            });
        }
        if !text.is_char_boundary(byte) {
            return Err(PositionError::InvalidUtf8Boundary(byte));
        }
        Ok(())
    }
}
