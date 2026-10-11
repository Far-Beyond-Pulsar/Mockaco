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
///
/// It is the document's [`Text`](crate::Text) itself: the rope keeps a line
/// index in its tree, so every conversion is O(log n) and nothing is rebuilt
/// when the text changes.
pub type PositionMap = crate::Text;
