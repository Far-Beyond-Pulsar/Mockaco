//! Framework-independent editor chrome: smooth-scroll physics, scrollbar
//! geometry and the minimap model.
//!
//! Everything here is plain data and arithmetic so the native view only has
//! to paint it, and the behavior can be unit tested without a window.

use crate::{PaintRow, SurfaceColor};

/// Exponential smoothing rate (1/seconds) for mouse-wheel notches. At 45 the
/// view covers 90% of a notch in ~50 ms (three frames at 60 Hz): it reads as
/// motion rather than a jump, without trailing behind the wheel. Scrollbar
/// drags, minimap drags and touchpads bypass it and track the pointer 1:1.
const SMOOTH_RATE: f32 = 45.0;
/// Distance (px) below which an animation snaps to its target.
const SNAP_DISTANCE: f32 = 0.2;

/// One axis of animated scrolling. `position` is what is painted; `target`
/// is where input has asked the view to be.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SmoothScroll {
    position: f32,
    target: f32,
    max: f32,
}

impl SmoothScroll {
    pub fn position(&self) -> f32 {
        self.position
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn max(&self) -> f32 {
        self.max
    }

    pub fn is_animating(&self) -> bool {
        (self.target - self.position).abs() > f32::EPSILON
    }

    /// Updates the scrollable range, clamping the current state into it.
    pub fn set_max(&mut self, max: f32) {
        self.max = max.max(0.0);
        self.position = self.position.clamp(0.0, self.max);
        self.target = self.target.clamp(0.0, self.max);
    }

    /// Adds `delta` to the target; the position glides toward it.
    pub fn scroll_by(&mut self, delta: f32) {
        self.target = (self.target + delta).clamp(0.0, self.max);
    }

    /// Animates to `value`.
    pub fn scroll_to(&mut self, value: f32) {
        self.target = value.clamp(0.0, self.max);
    }

    /// Moves immediately (scrollbar drags, document loads).
    pub fn jump_to(&mut self, value: f32) {
        let value = value.clamp(0.0, self.max);
        self.position = value;
        self.target = value;
    }

    /// Advances the animation by `dt` seconds; returns whether it is still
    /// moving (the caller should request another frame).
    pub fn step(&mut self, dt: f32) -> bool {
        let remaining = self.target - self.position;
        if remaining.abs() <= SNAP_DISTANCE {
            self.position = self.target;
            return false;
        }
        let factor = 1.0 - (-dt.max(0.0) * SMOOTH_RATE).exp();
        self.position += remaining * factor;
        if (self.target - self.position).abs() <= SNAP_DISTANCE {
            self.position = self.target;
            return false;
        }
        true
    }
}

/// Smallest scrollbar thumb, so it stays grabbable in huge documents.
pub const MIN_THUMB: f32 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    pub start: f32,
    pub length: f32,
}

/// Thumb for a scrollbar whose track is `track` px long. `content` is the
/// full scrollable extent (viewport included), `offset` the current scroll.
/// `None` means nothing scrolls and no bar should be drawn.
pub fn thumb(content: f32, viewport: f32, offset: f32, track: f32) -> Option<Thumb> {
    if track <= 0.0 || content <= viewport + 0.5 {
        return None;
    }
    let length = (track * viewport / content).clamp(MIN_THUMB.min(track), track);
    let travel = (track - length).max(0.0);
    let max_offset = (content - viewport).max(0.0);
    let start = if max_offset <= 0.0 {
        0.0
    } else {
        travel * (offset / max_offset).clamp(0.0, 1.0)
    };
    Some(Thumb { start, length })
}

/// Inverse of [`thumb`]: the scroll offset for a thumb placed at `start`.
pub fn offset_for_thumb(content: f32, viewport: f32, track: f32, length: f32, start: f32) -> f32 {
    let travel = (track - length).max(0.0);
    let max_offset = (content - viewport).max(0.0);
    if travel <= 0.0 {
        0.0
    } else {
        (start / travel).clamp(0.0, 1.0) * max_offset
    }
}

/// Pixels of minimap height per document line.
pub const MINIMAP_LINE_HEIGHT: f32 = 2.0;
/// Pixels of minimap width per source column.
pub const MINIMAP_CHAR_WIDTH: f32 = 1.0;
/// Source columns the minimap represents.
pub const MINIMAP_MAX_COLUMNS: usize = 110;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimapTone {
    Text,
    Comment,
    String,
    Number,
    Punctuation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinimapRun {
    pub start: u16,
    pub len: u16,
    pub tone: MinimapTone,
}

/// The colored runs of one source line, as the minimap draws it.
///
/// The minimap asks for the lines it shows each frame (a few hundred at
/// most), so it never scans the document: its cost is the same for ten lines
/// as for ten million.
pub fn minimap_line(line: &str, line_comment: Option<&str>) -> Vec<MinimapRun> {
    let mut runs = Vec::new();
    line_runs(line, line_comment, &mut runs);
    runs
}

fn line_runs(line: &str, line_comment: Option<&str>, runs: &mut Vec<MinimapRun>) {
    let mut column = 0usize;
    let mut quote: Option<char> = None;
    let mut previous_word = false;
    let mut current: Option<MinimapRun> = None;
    let flush = |run: &mut Option<MinimapRun>, runs: &mut Vec<MinimapRun>| {
        if let Some(run) = run.take() {
            runs.push(run);
        }
    };
    let trimmed_start = line.len() - line.trim_start().len();
    for (offset, character) in line.char_indices() {
        if column >= MINIMAP_MAX_COLUMNS {
            break;
        }
        if character == '\t' {
            flush(&mut current, runs);
            column += 4 - column % 4;
            previous_word = false;
            continue;
        }
        if character.is_whitespace() {
            flush(&mut current, runs);
            column += 1;
            previous_word = false;
            continue;
        }
        if quote.is_none()
            && offset >= trimmed_start
            && line_comment.is_some_and(|prefix| line[offset..].starts_with(prefix))
        {
            flush(&mut current, runs);
            let remaining = line[offset..]
                .trim_end()
                .chars()
                .take(MINIMAP_MAX_COLUMNS)
                .count();
            let len = remaining.min(MINIMAP_MAX_COLUMNS - column);
            runs.push(MinimapRun {
                start: column as u16,
                len: len as u16,
                tone: MinimapTone::Comment,
            });
            return;
        }
        let tone = if let Some(open) = quote {
            if character == open {
                quote = None;
            }
            MinimapTone::String
        } else if matches!(character, '"' | '\'' | '`') {
            quote = Some(character);
            MinimapTone::String
        } else if character.is_ascii_digit() && !previous_word {
            MinimapTone::Number
        } else if character.is_alphanumeric() || character == '_' {
            MinimapTone::Text
        } else {
            MinimapTone::Punctuation
        };
        previous_word = character.is_alphanumeric() || character == '_';
        match current.as_mut() {
            Some(run) if run.tone == tone && run.start as usize + run.len as usize == column => {
                run.len += 1;
            }
            _ => {
                flush(&mut current, runs);
                current = Some(MinimapRun {
                    start: column as u16,
                    len: 1,
                    tone,
                });
            }
        }
        column += 1;
    }
    flush(&mut current, runs);
}

/// Where the minimap content and viewport slider sit inside its track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinimapLayout {
    /// Pixels the minimap content is scrolled (zero while the whole document
    /// fits in the track).
    pub content_offset: f32,
    pub slider_top: f32,
    pub slider_height: f32,
    pub first_line: usize,
    /// Exclusive.
    pub last_line: usize,
}

pub fn minimap_layout(
    total_lines: usize,
    scroll: f32,
    max_scroll: f32,
    viewport: f32,
    line_height: f32,
    track: f32,
) -> MinimapLayout {
    let line_height = line_height.max(1.0);
    let total_height = total_lines as f32 * MINIMAP_LINE_HEIGHT;
    let content_offset = if total_height <= track || max_scroll <= 0.0 {
        0.0
    } else {
        (scroll / max_scroll).clamp(0.0, 1.0) * (total_height - track)
    };
    let slider_height = (viewport / line_height * MINIMAP_LINE_HEIGHT)
        .clamp(8.0_f32.min(track), track.max(0.0));
    let slider_top = ((scroll / line_height) * MINIMAP_LINE_HEIGHT - content_offset)
        .clamp(0.0, (track - slider_height).max(0.0));
    let first_line = (content_offset / MINIMAP_LINE_HEIGHT).floor() as usize;
    let last_line = (((content_offset + track) / MINIMAP_LINE_HEIGHT).ceil() as usize + 1)
        .min(total_lines);
    MinimapLayout {
        content_offset,
        slider_top,
        slider_height,
        first_line: first_line.min(last_line),
        last_line,
    }
}

/// Scroll offset that centers the document line under `pointer_y` (a
/// position inside the minimap track).
pub fn minimap_scroll_for_pointer(
    layout: &MinimapLayout,
    pointer_y: f32,
    max_scroll: f32,
    viewport: f32,
    line_height: f32,
) -> f32 {
    let line = (pointer_y + layout.content_offset) / MINIMAP_LINE_HEIGHT;
    let visible = viewport / line_height.max(1.0);
    ((line - visible / 2.0) * line_height).clamp(0.0, max_scroll.max(0.0))
}

/// Scroll offset for a slider dragged so its top sits at `slider_top`.
pub fn minimap_scroll_for_slider(
    total_lines: usize,
    slider_top: f32,
    slider_height: f32,
    max_scroll: f32,
    line_height: f32,
    track: f32,
) -> f32 {
    let total_height = total_lines as f32 * MINIMAP_LINE_HEIGHT;
    let value = if total_height <= track {
        slider_top / MINIMAP_LINE_HEIGHT * line_height
    } else {
        let travel = (track - slider_height).max(1.0);
        (slider_top / travel).clamp(0.0, 1.0) * max_scroll
    };
    value.clamp(0.0, max_scroll.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_scroll_converges_and_clamps() {
        let mut scroll = SmoothScroll::default();
        scroll.set_max(1000.0);
        scroll.scroll_by(400.0);
        assert!(scroll.is_animating());
        let mut frames = 0;
        while scroll.step(1.0 / 60.0) {
            frames += 1;
            assert!(frames < 240, "animation must settle");
        }
        assert_eq!(scroll.position(), 400.0);
        scroll.scroll_by(5000.0);
        assert_eq!(scroll.target(), 1000.0);
        scroll.set_max(300.0);
        assert_eq!(scroll.target(), 300.0);
        assert_eq!(scroll.position(), 300.0);
    }

    #[test]
    fn smooth_scroll_moves_partway_each_frame() {
        let mut scroll = SmoothScroll::default();
        scroll.set_max(1000.0);
        scroll.scroll_to(100.0);
        scroll.step(1.0 / 60.0);
        assert!(scroll.position() > 0.0 && scroll.position() < 100.0);
    }

    #[test]
    fn thumb_tracks_offset_and_round_trips() {
        let content = 4000.0;
        let viewport = 400.0;
        let track = 400.0;
        assert!(thumb(400.0, 400.0, 0.0, track).is_none());
        let top = thumb(content, viewport, 0.0, track).unwrap();
        assert_eq!(top.start, 0.0);
        assert!(top.length >= MIN_THUMB);
        let bottom = thumb(content, viewport, content - viewport, track).unwrap();
        assert!((bottom.start + bottom.length - track).abs() < 0.01);
        let offset = offset_for_thumb(content, viewport, track, bottom.length, bottom.start);
        assert!((offset - (content - viewport)).abs() < 0.5);
    }

    #[test]
    fn minimap_runs_classify_tones_and_comments() {
        let line = |text: &str| minimap_line(text, Some("//"));
        let first = line("let x = \"hi\"; // note");
        assert!(first.iter().any(|run| run.tone == MinimapTone::String));
        assert_eq!(first.last().unwrap().tone, MinimapTone::Comment);
        let comment = line("    // all comment");
        assert_eq!(comment[0].tone, MinimapTone::Comment);
        assert_eq!(comment[0].start, 4);
        assert_eq!(line("42 + y")[0].tone, MinimapTone::Number);
        assert!(line("").is_empty());
    }

    #[test]
    fn minimap_layout_keeps_slider_inside_track() {
        let layout = minimap_layout(10_000, 0.0, 200_000.0, 600.0, 20.0, 700.0);
        assert_eq!(layout.slider_top, 0.0);
        assert_eq!(layout.first_line, 0);
        let end = minimap_layout(10_000, 200_000.0, 200_000.0, 600.0, 20.0, 700.0);
        assert!((end.slider_top + end.slider_height - 700.0).abs() < 0.01);
        assert_eq!(end.last_line, 10_000);
        let short = minimap_layout(50, 0.0, 980.0, 600.0, 20.0, 700.0);
        assert_eq!(short.content_offset, 0.0);
        assert_eq!(short.last_line, 50);
    }

    #[test]
    fn minimap_pointer_centers_the_line() {
        let layout = minimap_layout(100, 0.0, 1980.0, 400.0, 20.0, 700.0);
        let scroll = minimap_scroll_for_pointer(&layout, 100.0, 1980.0, 400.0, 20.0);
        // pointer at minimap line 50, window shows 20 lines -> top line 40.
        assert!((scroll - 800.0).abs() < 0.01);
    }
}

/// One contiguous run of identically styled text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSpan {
    /// Length in bytes of the (tab-expanded) text.
    pub len: usize,
    pub foreground: Option<SurfaceColor>,
    pub bold: bool,
    pub italic: bool,
}

/// A row's visible text, ready to hand to a text shaper: tabs are expanded to
/// spaces so one character is exactly one grid column, and only the columns
/// that can be on screen are included.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RowText {
    pub text: String,
    pub spans: Vec<TextSpan>,
    /// Display column of the first character in `text`.
    pub start_column: usize,
}

/// Builds the paintable text for `row` limited to
/// `first_column..first_column + max_columns`. Overlapping syntax tokens are
/// resolved so the innermost (shortest) token styles each character.
pub fn layout_row_text(
    row: &PaintRow,
    tab_width: usize,
    first_column: usize,
    max_columns: usize,
) -> RowText {
    let tab_width = tab_width.max(1);
    let end_column = first_column.saturating_add(max_columns);
    let tokens = &row.tokens;
    let base = row.source_range.start;
    let mut next_token = 0usize;
    let mut active: Vec<usize> = Vec::new();
    let mut output = RowText {
        start_column: first_column,
        ..RowText::default()
    };
    let mut started = false;
    let mut column = 0usize;
    for (offset, character) in row.text.char_indices() {
        let width = if character == '\t' {
            tab_width - column % tab_width
        } else {
            1
        };
        let cell_start = column;
        column += width;
        if character == '\r' {
            continue;
        }
        if column <= first_column {
            continue;
        }
        if cell_start >= end_column {
            break;
        }
        let byte = base + offset;
        while next_token < tokens.len() && tokens[next_token].range.start <= byte {
            active.push(next_token);
            next_token += 1;
        }
        active.retain(|index| tokens[*index].range.end > byte);
        let style = active
            .iter()
            .filter(|index| tokens[**index].style.is_some())
            .min_by_key(|index| tokens[**index].range.end - tokens[**index].range.start)
            .and_then(|index| tokens[*index].style);
        let visible_start = cell_start.max(first_column);
        let visible_end = column.min(end_column);
        if !started {
            output.start_column = visible_start;
            started = true;
        }
        let (foreground, bold, italic) = match style {
            Some(style) => (
                Some(SurfaceColor::rgba(
                    style.foreground.red,
                    style.foreground.green,
                    style.foreground.blue,
                    style.foreground.alpha,
                )),
                style.font.bold,
                style.font.italic,
            ),
            None => (None, false, false),
        };
        let before = output.text.len();
        if character == '\t' {
            output
                .text
                .extend(std::iter::repeat(' ').take(visible_end - visible_start));
        } else {
            output.text.push(character);
        }
        let len = output.text.len() - before;
        match output.spans.last_mut() {
            Some(span)
                if span.foreground == foreground && span.bold == bold && span.italic == italic =>
            {
                span.len += len;
            }
            _ => output.spans.push(TextSpan {
                len,
                foreground,
                bold,
                italic,
            }),
        }
    }
    output
}

/// Display width of `text` with tabs expanded (one column per other char).
pub fn display_width(text: &str, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    text.chars().fold(0, |column, character| {
        column
            + if character == '\t' {
                tab_width - column % tab_width
            } else if character == '\r' {
                0
            } else {
                1
            }
    })
}

#[cfg(test)]
mod row_text_tests {
    use super::*;
    use crate::PaintToken;
    use mockaco_language::{FontStyle, Rgba, TokenKind, TokenStyle};

    fn style(red: u8) -> Option<TokenStyle> {
        Some(TokenStyle {
            foreground: Rgba::rgb(red, 0, 0),
            background: None,
            font: FontStyle::default(),
        })
    }

    fn row(text: &str, tokens: Vec<PaintToken>) -> PaintRow {
        PaintRow {
            display_row: 0,
            buffer_line: 0,
            source_range: 0..text.len(),
            text: text.to_owned(),
            y: 0.0,
            height: 20.0,
            continuation: false,
            folded: false,
            truncated: false,
            active: false,
            indent_guides: Vec::new(),
            tokens,
        }
    }

    fn token(range: std::ops::Range<usize>, red: u8) -> PaintToken {
        PaintToken {
            range,
            kind: TokenKind::new("k"),
            style: style(red),
        }
    }

    #[test]
    fn tabs_expand_to_spaces_and_columns_match_the_display_map() {
        let text = layout_row_text(&row("\tx", Vec::new()), 4, 0, 100);
        assert_eq!(text.text, "    x");
        assert_eq!(display_width("\tx", 4), 5);
    }

    #[test]
    fn inner_tokens_win_over_enclosing_ones() {
        let row = row("abcdef", vec![token(0..6, 1), token(2..4, 2)]);
        let text = layout_row_text(&row, 4, 0, 100);
        let reds: Vec<u8> = text
            .spans
            .iter()
            .map(|span| span.foreground.unwrap().red)
            .collect();
        assert_eq!(reds, vec![1, 2, 1]);
        assert_eq!(text.spans.iter().map(|s| s.len).sum::<usize>(), 6);
    }

    #[test]
    fn column_window_clips_long_lines_and_partial_tabs() {
        let long = "x".repeat(10_000);
        let text = layout_row_text(&row(&long, Vec::new()), 4, 100, 50);
        assert_eq!(text.start_column, 100);
        assert_eq!(text.text.len(), 50);
        let tabbed = layout_row_text(&row("\tab", Vec::new()), 4, 2, 10);
        assert_eq!(tabbed.start_column, 2);
        assert_eq!(tabbed.text, "  ab");
    }
}

/// Pre-computed minimap content with an overscroll buffer.
///
/// The rows covering the minimap's height plus half that height above and
/// half below are derived once and kept. Each frame only shifts them by how
/// far the minimap has scrolled since; they are rebuilt, re-centred, once
/// the shift passes a quarter of the height (halfway into the margin) or the
/// content changes. The margin is only pre-computed content: the minimap
/// never scrolls past the document, and rows outside it are simply absent.
#[derive(Debug, Clone)]
pub struct MinimapBuffer<K> {
    key: K,
    /// Minimap scroll (px) the rows were built around.
    anchor: f32,
    height: f32,
    /// `(display row, runs)` for every row in the buffered span.
    pub rows: Vec<(usize, Vec<MinimapRun>)>,
}

impl<K: PartialEq + Clone> MinimapBuffer<K> {
    /// Whether this buffer can serve scroll `offset` for a minimap `height`
    /// tall whose content is `key`.
    pub fn serves(&self, key: &K, offset: f32, height: f32) -> bool {
        self.key == *key
            && (self.height - height).abs() < 0.5
            && (offset - self.anchor).abs() <= height * 0.25
    }

    /// The display rows a buffer built at `offset` covers: the visible span
    /// plus half the height above and below, clamped to the document.
    pub fn span(offset: f32, height: f32, row_count: usize) -> std::ops::Range<usize> {
        let margin = height * 0.5;
        let first = ((offset - margin) / MINIMAP_LINE_HEIGHT).floor().max(0.0) as usize;
        let last = (((offset + height + margin) / MINIMAP_LINE_HEIGHT).ceil() as usize + 1)
            .min(row_count);
        first.min(last)..last
    }

    pub fn new(key: K, offset: f32, height: f32, rows: Vec<(usize, Vec<MinimapRun>)>) -> Self {
        Self {
            key,
            anchor: offset,
            height,
            rows,
        }
    }
}

#[cfg(test)]
mod minimap_buffer_tests {
    use super::*;

    #[test]
    fn refills_only_past_a_quarter_of_the_height() {
        let buffer = MinimapBuffer::new(1u64, 1000.0, 400.0, Vec::new());
        assert!(buffer.serves(&1, 1000.0, 400.0));
        assert!(buffer.serves(&1, 1100.0, 400.0), "100 px is within 25%");
        assert!(buffer.serves(&1, 900.0, 400.0), "both directions");
        assert!(!buffer.serves(&1, 1101.0, 400.0), "past 25% refills");
        assert!(!buffer.serves(&2, 1000.0, 400.0), "content changed");
        assert!(!buffer.serves(&1, 1000.0, 300.0), "resized");
    }

    #[test]
    fn span_covers_half_a_height_each_side_within_the_document() {
        // At 1000 px the top row is 500; 400 px tall at 2 px/row shows 200
        // rows, and 100 more are kept on each side: rows 400..800.
        let span = MinimapBuffer::<u64>::span(1000.0, 400.0, 100_000);
        assert_eq!(span.start, 400);
        assert!((800..=802).contains(&span.end));
        // Never above the first row or past the last.
        assert_eq!(MinimapBuffer::<u64>::span(0.0, 400.0, 100_000).start, 0);
        assert_eq!(MinimapBuffer::<u64>::span(1000.0, 400.0, 50).end, 50);
    }
}
