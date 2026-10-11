//! Native WGPUI (gpui-ce) editor widget.
//!
//! `WgpuiEditorView` is the complete editor: it owns the surface, input
//! routing, syntax-colored text, gutter with fold controls, minimap,
//! scrollbars and smooth scrolling. Hosts embed the entity and listen for
//! [`EditorEvent`]s; they supply a theme, a font and a language and nothing
//! else. All painting goes through one canvas-style element so text, caret,
//! selection and hit testing share a single set of measured metrics.

use crate::chrome::{
    self, layout_row_text, MinimapLayout, MinimapRun, MinimapTone, RowText, SmoothScroll,
    Thumb, MINIMAP_CHAR_WIDTH, MINIMAP_LINE_HEIGHT, MINIMAP_MAX_COLUMNS,
};
use crate::{
    EditCommand, EditorSurface, InputEvent, InputRouter, Key, KeyEvent, KeyModifiers, Language,
    MouseButton, MouseEvent, PaintRow, RenderFrame, SurfaceColor, SurfaceGeometry, SurfaceTheme,
    CODE_PADDING, GUTTER_FOLD_WIDTH, GUTTER_MARKER_WIDTH,
};
use mockaco_core::{Affinity, SelectionSet};
use mockaco_renderer::{
    DIAGNOSTIC_ERROR_STYLE_ID, DIAGNOSTIC_HINT_STYLE_ID, DIAGNOSTIC_INFO_STYLE_ID,
    DIAGNOSTIC_WARNING_STYLE_ID, SEARCH_CURRENT_MATCH_STYLE_ID, SEARCH_MATCH_STYLE_ID,
};
use std::collections::HashMap;
use std::time::Instant;
use wgpui::{
    canvas, div, fill, font, inflate_for_buffer, point, prepare_scroll_buffer, px, size,
    with_buffer_mask, App, Bounds, ClipboardItem, ContentMask, Context, Corners,
    CursorStyle, DispatchPhase, Element, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, InteractiveElement, IntoElement, KeyDownEvent,
    LayerPolicy, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    PathBuilder, Pixels, ScrollBufferFrame, StatefulInteractiveElement,
    Point, Render, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Size, Style, Styled,
    TextRun, Window,
};

const VBAR_WIDTH: f32 = 14.0;
const HBAR_HEIGHT: f32 = 12.0;
const THUMB_THICKNESS: f32 = 8.0;
const MINIMAP_PADDING: f32 = 8.0;
/// The minimap is hidden below this editor width.
const MINIMAP_MIN_EDITOR_WIDTH: f32 = 480.0;
/// Lines scrolled per mouse-wheel notch.
const WHEEL_LINES: f32 = 3.0;
/// Extra columns of horizontal range matching the surface's scroll slack.
const COLUMN_SLACK: usize = 6;

/// Events a host can subscribe to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    /// The document text changed (typing, paste, undo, ...).
    Changed,
    /// The user pressed Ctrl/Cmd+S.
    SaveRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    None,
    Gutter,
    Text,
    VScroll,
    HScroll,
    Minimap,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    None,
    Text,
    VScroll { grab: f32 },
    HScroll { grab: f32 },
    Minimap { grab: f32 },
}

/// Geometry of the editor's regions for the current frame, in editor-local
/// pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ChromeLayout {
    width: f32,
    height: f32,
    gutter: f32,
    text_left: f32,
    /// Right edge of the code region (the minimap/scrollbar start here).
    code_right: f32,
    /// Height of the text viewport (excludes the horizontal scrollbar).
    viewport_height: f32,
    minimap_left: f32,
    minimap_width: f32,
    vbar_left: f32,
    hbar_height: f32,
}

pub struct WgpuiEditorView {
    pub surface: EditorSurface,
    pub input_router: InputRouter,
    pub read_only: bool,
    focus_handle: FocusHandle,
    font_family: String,
    font_size: f32,
    line_height: f32,
    char_width: f32,
    background: Hsla,
    /// Bumped when the theme changes, so cached layers repaint.
    theme_revision: u64,
    y_scroll: SmoothScroll,
    x_scroll: SmoothScroll,
    last_frame: Option<Instant>,
    layout: ChromeLayout,
    drag: Drag,
    hover: Region,
    show_minimap: bool,
    line_backgrounds: HashMap<usize, SurfaceColor>,
    /// Show fold chevrons on every foldable line (true) or only while the
    /// pointer is over the gutter (false).
    always_show_fold_controls: bool,
}

impl EventEmitter<EditorEvent> for WgpuiEditorView {}

impl Focusable for WgpuiEditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl WgpuiEditorView {
    pub fn new(surface: EditorSurface, cx: &mut Context<Self>) -> Self {
        Self::build(surface, false, cx)
    }

    pub fn new_read_only(surface: EditorSurface, cx: &mut Context<Self>) -> Self {
        Self::build(surface, true, cx)
    }

    fn build(surface: EditorSurface, read_only: bool, cx: &mut Context<Self>) -> Self {
        let geometry = surface.geometry();
        Self {
            background: hsla(surface.theme().background),
            theme_revision: 0,
            surface,
            input_router: InputRouter::default(),
            read_only,
            focus_handle: cx.focus_handle(),
            font_family: "JetBrains Mono".to_owned(),
            font_size: 14.0,
            line_height: geometry.line_height.max(1.0),
            char_width: geometry.character_width.max(1.0),
            y_scroll: SmoothScroll::default(),
            x_scroll: SmoothScroll::default(),
            last_frame: None,
            layout: ChromeLayout::default(),
            drag: Drag::None,
            hover: Region::None,
            show_minimap: true,
            line_backgrounds: HashMap::new(),
            always_show_fold_controls: true,
        }
    }

    // ---- host API -------------------------------------------------------

    /// The whole document as one string: O(document size). For saving and
    /// handing the file to other tools, never per keystroke or per frame.
    pub fn text(&self) -> String {
        self.surface.document().text().to_string()
    }

    pub fn render_frame(&self) -> RenderFrame {
        self.surface.render_frame()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    pub fn set_font(&mut self, family: impl Into<String>, size: f32, line_height: f32) {
        self.font_family = family.into();
        self.font_size = size.max(6.0);
        self.line_height = line_height.max(self.font_size);
        // Force the next frame to re-measure and re-layout.
        self.char_width = 0.0;
    }

    pub fn set_theme(&mut self, theme: SurfaceTheme, cx: &mut Context<Self>) {
        if self.surface.theme() != theme {
            self.background = hsla(theme.background);
            self.surface.set_theme(theme);
            self.theme_revision = self.theme_revision.wrapping_add(1);
            cx.notify();
        }
    }

    pub fn set_language(&mut self, language: Language, cx: &mut Context<Self>) {
        self.surface.set_language(language);
        cx.notify();
    }

    pub fn set_show_minimap(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_minimap = show;
        cx.notify();
    }

    pub fn set_fold_controls_always_visible(&mut self, always: bool, cx: &mut Context<Self>) {
        self.always_show_fold_controls = always;
        cx.notify();
    }

    /// Buffer-line background tints (diff highlights, current-debug-line...).
    pub fn set_line_backgrounds(
        &mut self,
        backgrounds: HashMap<usize, SurfaceColor>,
        cx: &mut Context<Self>,
    ) {
        self.line_backgrounds = backgrounds;
        cx.notify();
    }

    /// Replaces the whole document (file loads, external changes).
    pub fn replace_text(&mut self, text: impl Into<mockaco_core::Text>, cx: &mut Context<Self>) {
        self.surface.replace_text(text);
        self.y_scroll.jump_to(0.0);
        self.x_scroll.jump_to(0.0);
        cx.notify();
    }

    /// Current scroll offset in pixels as `(x, y)`.
    pub fn scroll_offset(&self) -> (f32, f32) {
        (self.x_scroll.position(), self.y_scroll.position())
    }

    pub fn set_scroll_offset(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        self.x_scroll.jump_to(x);
        self.y_scroll.jump_to(y);
        cx.notify();
    }

    /// Moves the caret to zero-based `line`/`column` (in characters) and
    /// scrolls it to the middle of the viewport.
    pub fn go_to(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        let map = self.surface.document().position_map();
        let (Ok(start), Ok(end)) = (map.line_start(line), map.line_end(line)) else {
            return;
        };
        let text = self.surface.document().text();
        let caret = text
            .chars_from(start)
            .take_while(|(offset, _)| *offset < end)
            .nth(column)
            .map_or(end, |(offset, _)| offset);
        self.surface
            .editor_mut()
            .set_selections(SelectionSet::caret(caret));
        if let Ok(point) = self.surface.display().buffer_to_display(caret, Affinity::After) {
            let rows = self.layout.viewport_height / self.line_height;
            self.y_scroll
                .scroll_to((point.row as f32 - rows / 2.0 + 0.5).max(0.0) * self.line_height);
        }
        cx.notify();
    }

    // ---- commands (menus, toolbars) ----------------------------------------

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.undo_redo(false, cx);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.undo_redo(true, cx);
    }

    pub fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.surface.clipboard_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn cut(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        self.copy(cx);
        self.route(
            InputEvent::Key(KeyEvent {
                key: Key::Character("x".to_owned()),
                modifiers: KeyModifiers {
                    control: true,
                    ..KeyModifiers::default()
                },
            }),
            cx,
        );
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.route(InputEvent::Paste(text.replace("\r\n", "\n")), cx);
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.route(
            InputEvent::Key(KeyEvent {
                key: Key::Character("a".to_owned()),
                modifiers: KeyModifiers {
                    control: true,
                    ..KeyModifiers::default()
                },
            }),
            cx,
        );
    }

    pub fn unfold_all(&mut self, cx: &mut Context<Self>) {
        self.surface.unfold_all();
        cx.notify();
    }

    pub fn has_selection(&self) -> bool {
        self.surface
            .editor()
            .selections()
            .selections()
            .iter()
            .any(|selection| !selection.is_caret())
    }

    pub fn line_count(&self) -> usize {
        self.surface.document().position_map().line_count()
    }

    // ---- per-frame preparation -------------------------------------------

    fn editor_font(&self) -> Font {
        let mut editor_font = font(self.font_family.clone());
        editor_font.fallbacks = Some(FontFallbacks::from_fonts(vec![
            "Cascadia Mono".to_owned(),
            "Consolas".to_owned(),
            "Menlo".to_owned(),
            "DejaVu Sans Mono".to_owned(),
            "monospace".to_owned(),
        ]));
        // Ligatures would merge cells and break the one-char-one-column grid.
        editor_font.features = FontFeatures::disable_ligatures();
        editor_font
    }

    fn compute_layout(&self, width: f32, height: f32) -> ChromeLayout {
        let gutter = self.surface.gutter_width();
        let minimap_width = if self.show_minimap && width >= MINIMAP_MIN_EDITOR_WIDTH {
            MINIMAP_MAX_COLUMNS as f32 * MINIMAP_CHAR_WIDTH + MINIMAP_PADDING * 2.0
        } else {
            0.0
        };
        let code_right = (width - VBAR_WIDTH - minimap_width).max(gutter + self.char_width);
        let text_left = gutter + CODE_PADDING;
        let code_width = (code_right - text_left - 4.0).max(self.char_width);
        let content_width =
            (self.surface.display().max_display_width() + COLUMN_SLACK) as f32 * self.char_width;
        let hbar_height = if content_width > code_width + 0.5 {
            HBAR_HEIGHT
        } else {
            0.0
        };
        ChromeLayout {
            width,
            height,
            gutter,
            text_left,
            code_right,
            viewport_height: (height - hbar_height).max(self.line_height),
            minimap_left: code_right,
            minimap_width,
            vbar_left: width - VBAR_WIDTH,
            hbar_height,
        }
    }

    /// Called from the element's prepaint: measures the font, sizes the
    /// surface, advances scroll animation and syncs the surface to it.
    /// Returns whether another animation frame is needed.
    fn prepare(&mut self, width: f32, height: f32, window: &Window) -> bool {
        let editor_font = self.editor_font();
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&editor_font);
        if let Ok(advance) = text_system.ch_advance(font_id, px(self.font_size)) {
            let measured = advance.to_f32();
            if measured.is_finite() && measured > 0.0 && (measured - self.char_width).abs() > 0.001
            {
                self.char_width = measured;
            }
        }
        if self.char_width <= 0.0 {
            self.char_width = self.font_size * 0.6;
        }

        let mut layout = self.compute_layout(width, height);
        let geometry = SurfaceGeometry {
            width: (layout.code_right - layout.text_left - 4.0).max(self.char_width),
            height: layout.viewport_height,
            line_height: self.line_height,
            character_width: self.char_width,
            gutter_character_width: self.char_width,
        };
        if self.surface.geometry() != geometry {
            self.surface.set_geometry(geometry);
            // The gutter width depends on the line-number digits; recompute
            // so the layout always matches the surface.
            layout = self.compute_layout(width, height);
        }
        self.layout = layout;

        let scroll = self.surface.scroll();
        self.y_scroll.set_max(
            scroll.content_rows.saturating_sub(scroll.viewport_rows) as f32 * self.line_height,
        );
        self.x_scroll.set_max(
            scroll.content_columns.saturating_sub(scroll.viewport_columns) as f32
                * self.char_width,
        );

        let now = Instant::now();
        let dt = self
            .last_frame
            .map_or(1.0 / 60.0, |last| now.duration_since(last).as_secs_f32())
            .clamp(1.0 / 240.0, 1.0 / 15.0);
        let moving_y = self.y_scroll.step(dt);
        let moving_x = self.x_scroll.step(dt);
        let animating = moving_y || moving_x;
        self.last_frame = animating.then_some(now);
        self.apply_scroll_to_surface();

        // Highlights and folds arrive from the language worker; apply what
        // is ready, and keep frames coming while it is still working.
        self.surface.poll_language();
        animating || self.surface.language_pending()
    }

    /// Whole rows/columns of the animated position drive the surface; the
    /// sub-row/sub-column remainder is applied as a paint-time translation.
    fn apply_scroll_to_surface(&mut self) {
        let row = (self.y_scroll.position() / self.line_height).floor().max(0.0) as usize;
        let column = (self.x_scroll.position() / self.char_width).floor().max(0.0) as usize;
        self.surface.scroll_to(row, column);
    }

    fn remainders(&self) -> (f32, f32) {
        let scroll = self.surface.scroll();
        (
            (self.x_scroll.position() - scroll.horizontal_columns as f32 * self.char_width)
                .max(0.0),
            (self.y_scroll.position() - scroll.top_row as f32 * self.line_height).max(0.0),
        )
    }

    /// Aligns the surface with where scrolling is headed, so caret-reveal
    /// logic reasons about the destination rather than a mid-animation frame.
    fn sync_surface_to_target(&mut self) {
        let row = (self.y_scroll.target() / self.line_height).floor().max(0.0) as usize;
        let column = (self.x_scroll.target() / self.char_width).floor().max(0.0) as usize;
        self.surface.scroll_to(row, column);
    }

    fn retarget_from_surface(&mut self) {
        let scroll = self.surface.scroll();
        self.y_scroll
            .scroll_to(scroll.top_row as f32 * self.line_height);
        self.x_scroll
            .scroll_to(scroll.horizontal_columns as f32 * self.char_width);
    }

    // ---- input -----------------------------------------------------------

    fn is_edit(command: &EditCommand) -> bool {
        matches!(
            command,
            EditCommand::InsertText(_)
                | EditCommand::Cut
                | EditCommand::Paste(_)
                | EditCommand::DeleteBackward
                | EditCommand::DeleteForward
                | EditCommand::DeleteWordBackward
                | EditCommand::DeleteWordForward
                | EditCommand::Newline
                | EditCommand::Tab
                | EditCommand::Outdent
        )
    }

    fn route(&mut self, event: InputEvent, cx: &mut Context<Self>) {
        if self.read_only {
            let blocked = match &event {
                InputEvent::Text(_) | InputEvent::Paste(_) | InputEvent::Ime(_) => true,
                InputEvent::Key(key) => InputRouter::translate_key(key)
                    .ok()
                    .flatten()
                    .is_some_and(|command| Self::is_edit(&command)),
                _ => false,
            };
            if blocked {
                return;
            }
        }
        let keyboard = matches!(
            event,
            InputEvent::Key(_) | InputEvent::Text(_) | InputEvent::Paste(_)
        );
        if keyboard {
            self.sync_surface_to_target();
        }
        let is_scroll = matches!(event, InputEvent::Scroll { .. });
        match self.input_router.route(&mut self.surface, event) {
            Ok(outcome) => {
                if outcome.document_changed {
                    cx.emit(EditorEvent::Changed);
                }
                // Only keyboard navigation retargets scrolling; a mouse click
                // never shifts the view (the paint-time sync restores it).
                if outcome.scroll_changed && !is_scroll && keyboard {
                    self.retarget_from_surface();
                }
                cx.notify();
            }
            Err(_) => {}
        }
    }

    fn undo_redo(&mut self, redo: bool, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        self.sync_surface_to_target();
        let changed = if redo {
            self.surface.redo()
        } else {
            self.surface.undo()
        };
        if changed {
            self.surface.reveal_primary_caret();
            self.retarget_from_surface();
            cx.emit(EditorEvent::Changed);
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = KeyModifiers {
            shift: keystroke.modifiers.shift,
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            command: keystroke.modifiers.platform,
        };
        let primary = modifiers.control || modifiers.command;
        if primary && !modifiers.alt {
            match keystroke.key.as_str() {
                "s" => {
                    cx.emit(EditorEvent::SaveRequested);
                    cx.stop_propagation();
                    return;
                }
                "z" => {
                    self.undo_redo(modifiers.shift, cx);
                    cx.stop_propagation();
                    return;
                }
                "y" => {
                    self.undo_redo(true, cx);
                    cx.stop_propagation();
                    return;
                }
                "c" => {
                    cx.write_to_clipboard(ClipboardItem::new_string(self.surface.clipboard_text()));
                    cx.stop_propagation();
                    return;
                }
                "x" => {
                    if !self.read_only {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.surface.clipboard_text(),
                        ));
                        self.route(
                            InputEvent::Key(KeyEvent {
                                key: Key::Character("x".to_owned()),
                                modifiers,
                            }),
                            cx,
                        );
                    }
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    if !self.read_only {
                        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                            self.route(InputEvent::Paste(text.replace("\r\n", "\n")), cx);
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }

        let key = match keystroke.key.as_str() {
            "backspace" => Key::Backspace,
            "delete" => Key::Delete,
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "escape" => Key::Escape,
            "left" => Key::Left,
            "right" => Key::Right,
            "up" => Key::Up,
            "down" => Key::Down,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "home" => Key::Home,
            "end" => Key::End,
            other => {
                if primary {
                    if other.chars().count() == 1 {
                        Key::Character(other.to_owned())
                    } else {
                        Key::Unsupported(other.to_owned())
                    }
                } else if let Some(text) = keystroke.key_char.clone() {
                    self.route(InputEvent::Text(text), cx);
                    cx.stop_propagation();
                    return;
                } else {
                    Key::Unsupported(other.to_owned())
                }
            }
        };
        let handled = InputRouter::translate_key(&KeyEvent {
            key: key.clone(),
            modifiers,
        })
        .map(|command| command.is_some())
        .unwrap_or(false);
        self.route(InputEvent::Key(KeyEvent { key, modifiers }), cx);
        if handled {
            cx.stop_propagation();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let (mut dx, mut dy, precise) = match event.delta {
            // Whole-number line deltas are mouse-wheel notches: scroll a few
            // lines and glide. Fractional ones come from precision
            // touchpads, which stream many small deltas (plus inertia) and
            // must be followed directly or the glide fights the stream.
            ScrollDelta::Lines(delta) => {
                let notch = delta.x.fract() == 0.0 && delta.y.fract() == 0.0;
                if notch {
                    (
                        -delta.x * self.char_width * WHEEL_LINES,
                        -delta.y * self.line_height * WHEEL_LINES,
                        false,
                    )
                } else {
                    (
                        -delta.x * self.char_width,
                        -delta.y * self.line_height,
                        true,
                    )
                }
            }
            ScrollDelta::Pixels(delta) => (-delta.x.to_f32(), -delta.y.to_f32(), true),
        };
        if event.modifiers.shift && dx == 0.0 {
            std::mem::swap(&mut dx, &mut dy);
        }
        if precise {
            // Touchpads already deliver smooth deltas; follow them directly.
            self.y_scroll.jump_to(self.y_scroll.target() + dy);
            self.x_scroll.jump_to(self.x_scroll.target() + dx);
        } else {
            self.y_scroll.scroll_by(dy);
            self.x_scroll.scroll_by(dx);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn region_at(&self, x: f32, y: f32) -> Region {
        let layout = &self.layout;
        if x >= layout.vbar_left {
            Region::VScroll
        } else if layout.minimap_width > 0.0 && x >= layout.minimap_left {
            Region::Minimap
        } else if layout.hbar_height > 0.0 && y >= layout.viewport_height {
            Region::HScroll
        } else if x < layout.gutter {
            Region::Gutter
        } else {
            Region::Text
        }
    }

    fn vertical_thumb(&self) -> Option<Thumb> {
        let viewport = self.layout.viewport_height;
        chrome::thumb(
            self.y_scroll.max() + viewport,
            viewport,
            self.y_scroll.position(),
            viewport,
        )
    }

    fn horizontal_thumb(&self) -> Option<Thumb> {
        let track = self.layout.code_right - self.layout.gutter;
        let viewport = self.layout.code_right - self.layout.text_left;
        chrome::thumb(
            self.x_scroll.max() + viewport,
            viewport,
            self.x_scroll.position(),
            track,
        )
    }

    fn minimap_layout(&self) -> MinimapLayout {
        chrome::minimap_layout(
            self.surface.display().row_count(),
            self.y_scroll.position(),
            self.y_scroll.max(),
            self.layout.viewport_height,
            self.line_height,
            self.layout.viewport_height,
        )
    }

    fn mouse_down(
        &mut self,
        x: f32,
        y: f32,
        click_count: usize,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let region = self.region_at(x, y);
        match region {
            Region::VScroll => {
                if let Some(thumb) = self.vertical_thumb() {
                    let grab = if (thumb.start..=thumb.start + thumb.length).contains(&y) {
                        y - thumb.start
                    } else {
                        thumb.length / 2.0
                    };
                    self.drag = Drag::VScroll { grab };
                    self.drag_vertical(y, grab);
                }
            }
            Region::HScroll => {
                if let Some(thumb) = self.horizontal_thumb() {
                    let local = x - self.layout.gutter;
                    let grab = if (thumb.start..=thumb.start + thumb.length).contains(&local) {
                        local - thumb.start
                    } else {
                        thumb.length / 2.0
                    };
                    self.drag = Drag::HScroll { grab };
                    self.drag_horizontal(local, grab);
                }
            }
            Region::Minimap => {
                let layout = self.minimap_layout();
                let grab = if (layout.slider_top..=layout.slider_top + layout.slider_height)
                    .contains(&y)
                {
                    y - layout.slider_top
                } else {
                    let target = chrome::minimap_scroll_for_pointer(
                        &layout,
                        y,
                        self.y_scroll.max(),
                        self.layout.viewport_height,
                        self.line_height,
                    );
                    self.y_scroll.scroll_to(target);
                    layout.slider_height / 2.0
                };
                self.drag = Drag::Minimap { grab };
            }
            Region::Gutter | Region::Text => {
                if y >= self.layout.viewport_height {
                    return;
                }
                let (rem_x, rem_y) = self.remainders();
                let (px_x, px_y) = (x + rem_x, y + rem_y);
                let event = if shift && region == Region::Text {
                    MouseEvent::Extend { x: px_x, y: px_y }
                } else {
                    MouseEvent::Down {
                        x: px_x,
                        y: px_y,
                        button: MouseButton::Primary,
                        click_count,
                    }
                };
                if region == Region::Text {
                    self.drag = Drag::Text;
                }
                self.route(InputEvent::Mouse(event), cx);
            }
            Region::None => {}
        }
        cx.notify();
    }

    fn drag_vertical(&mut self, y: f32, grab: f32) {
        if let Some(thumb) = self.vertical_thumb() {
            let viewport = self.layout.viewport_height;
            let offset = chrome::offset_for_thumb(
                self.y_scroll.max() + viewport,
                viewport,
                viewport,
                thumb.length,
                y - grab,
            );
            self.y_scroll.jump_to(offset);
        }
    }

    fn drag_horizontal(&mut self, local_x: f32, grab: f32) {
        if let Some(thumb) = self.horizontal_thumb() {
            let track = self.layout.code_right - self.layout.gutter;
            let viewport = self.layout.code_right - self.layout.text_left;
            let offset = chrome::offset_for_thumb(
                self.x_scroll.max() + viewport,
                viewport,
                track,
                thumb.length,
                local_x - grab,
            );
            self.x_scroll.jump_to(offset);
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32, left_down: bool, cx: &mut Context<Self>) {
        if self.drag != Drag::None && !left_down {
            self.drag = Drag::None;
        }
        match self.drag {
            Drag::None => {
                let region = if x >= 0.0 && y >= 0.0 && x < self.layout.width && y < self.layout.height
                {
                    self.region_at(x, y)
                } else {
                    Region::None
                };
                if region != self.hover {
                    self.hover = region;
                    cx.notify();
                }
            }
            Drag::Text => {
                // Auto-scroll while the pointer is beyond the viewport.
                let viewport = self.layout.viewport_height;
                if y < 0.0 {
                    self.y_scroll.jump_to(self.y_scroll.target() + y.max(-60.0) * 0.5);
                } else if y > viewport {
                    self.y_scroll
                        .jump_to(self.y_scroll.target() + (y - viewport).min(60.0) * 0.5);
                }
                if x < self.layout.text_left {
                    self.x_scroll
                        .jump_to(self.x_scroll.target() - (self.layout.text_left - x).min(60.0) * 0.5);
                } else if x > self.layout.code_right {
                    self.x_scroll.jump_to(
                        self.x_scroll.target() + (x - self.layout.code_right).min(60.0) * 0.5,
                    );
                }
                self.apply_scroll_to_surface();
                let (rem_x, rem_y) = self.remainders();
                self.route(
                    InputEvent::Mouse(MouseEvent::Drag {
                        x: x + rem_x,
                        y: y + rem_y,
                    }),
                    cx,
                );
            }
            Drag::VScroll { grab } => self.drag_vertical(y, grab),
            Drag::HScroll { grab } => self.drag_horizontal(x - self.layout.gutter, grab),
            Drag::Minimap { grab } => {
                let layout = self.minimap_layout();
                let target = chrome::minimap_scroll_for_slider(
                    self.surface.display().row_count(),
                    y - grab,
                    layout.slider_height,
                    self.y_scroll.max(),
                    self.line_height,
                    self.layout.viewport_height,
                );
                self.y_scroll.jump_to(target);
            }
        }
        if self.drag != Drag::None {
            cx.notify();
        }
    }

    /// Returns whether a drag was in progress (and so the event is consumed).
    fn mouse_up(&mut self, cx: &mut Context<Self>) -> bool {
        let was = std::mem::replace(&mut self.drag, Drag::None);
        if was == Drag::Text {
            self.route(
                InputEvent::Mouse(MouseEvent::Up {
                    x: 0.0,
                    y: 0.0,
                    button: MouseButton::Primary,
                }),
                cx,
            );
        }
        if was != Drag::None {
            cx.notify();
        }
        was != Drag::None
    }

    // ---- paint data ------------------------------------------------------
    /// The minimap runs of display rows `rows`, reading only those lines
    /// (and at most what the minimap can show of each).
    fn minimap_rows(&self, rows: std::ops::Range<usize>) -> Vec<(usize, Vec<MinimapRun>)> {
        let display = self.surface.display();
        let snapshot = self.surface.document();
        let text = snapshot.text();
        let comment = self.surface.language().line_comment();
        display
            .rows_in(rows.clone())
            .into_iter()
            .zip(rows)
            .filter(|(row, _)| !row.continuation)
            .filter_map(|(row, index)| {
                let end = text.line_end(row.buffer_line).ok()?;
                let source: String = text
                    .chars_from(row.start_byte)
                    .take_while(|(offset, _)| *offset < end)
                    .take(MINIMAP_MAX_COLUMNS * 2)
                    .map(|(_, c)| c)
                    .collect();
                Some((index, chrome::minimap_line(&source, comment)))
            })
            .collect()
    }

    /// What the minimap's content depends on, scroll excepted: while this is
    /// unchanged its buffered layer only shifts.
    fn minimap_key(&self) -> (u64, u64, u64, &'static str) {
        (
            self.surface.document().version(),
            self.surface.display().revision(),
            self.theme_revision,
            self.surface.language().id(),
        )
    }


    fn paint_data(&self, focused: bool) -> PaintData {
        let frame = self.surface.render_frame_with_buffer(1);
        let (rem_x, rem_y) = self.remainders();
        let scroll = self.surface.scroll();
        let tab_width = self.surface.display().config().tab_width;
        let backgrounds = frame
            .rows
            .iter()
            .map(|row| self.line_backgrounds.get(&row.buffer_line).copied())
            .collect();
        PaintData {
            layout: self.layout,
            frame,
            line_height: self.line_height,
            char_width: self.char_width,
            font_size: self.font_size,
            font: self.editor_font(),
            tab_width,
            rem_x,
            rem_y,
            first_column: scroll.horizontal_columns,
            viewport_columns: scroll.viewport_columns,
            backgrounds,
            focused,
            hover: self.hover,
            dragging: self.drag,
            vthumb: self.vertical_thumb(),
            hthumb: self.horizontal_thumb(),
            always_show_fold_controls: self.always_show_fold_controls,
        }
    }
}

struct PaintData {
    layout: ChromeLayout,
    frame: RenderFrame,
    line_height: f32,
    char_width: f32,
    font_size: f32,
    font: Font,
    tab_width: usize,
    rem_x: f32,
    rem_y: f32,
    first_column: usize,
    viewport_columns: usize,
    backgrounds: Vec<Option<SurfaceColor>>,
    focused: bool,
    hover: Region,
    dragging: Drag,
    vthumb: Option<Thumb>,
    hthumb: Option<Thumb>,
    always_show_fold_controls: bool,
}

impl Render for WgpuiEditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.entity();
        let layout = self.layout;
        let mut root = div()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(self.background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(EditorElement {
                editor: editor.clone(),
            });
        if layout.minimap_width > 0.0 {
            // Half the minimap's height pre-rendered above and below.
            let margin = (layout.viewport_height * 0.5).floor().max(0.0);
            let place = |element: wgpui::Div| {
                element
                    .absolute()
                    .top_0()
                    .right(px(VBAR_WIDTH))
                    .w(px(layout.minimap_width))
                    .h(px(layout.viewport_height))
            };
            root = root
                .child(
                    place(div())
                        .id("mockaco-minimap")
                        .layer_keyed(self.minimap_key())
                        .layer_with_policy(LayerPolicy {
                            overdraw_margin: size(px(0.), px(margin)),
                            ..LayerPolicy::default()
                        })
                        .child(minimap_canvas(editor.clone())),
                )
                .child(place(div()).child(minimap_overlay(editor)));
        }
        root
    }
}

// ---- the element -----------------------------------------------------------

struct EditorElement {
    editor: Entity<WgpuiEditorView>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let width = bounds.size.width.to_f32().max(1.0);
        let height = bounds.size.height.to_f32().max(1.0);
        let animating = self
            .editor
            .update(cx, |view, _| view.prepare(width, height, window));
        if animating {
            window.request_animation_frame();
        }
        let layout = self.editor.read(cx).layout;
        window.insert_hitbox(
            Bounds {
                origin: point(bounds.origin.x + px(layout.gutter), bounds.origin.y),
                size: size(
                    px((layout.code_right - layout.gutter).max(0.0)),
                    px(layout.viewport_height),
                ),
            },
            HitboxBehavior::Normal,
        )
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        text_hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focused = self.editor.read(cx).focus_handle.is_focused(window);
        let data = self.editor.read(cx).paint_data(focused);
        window.set_cursor_style(CursorStyle::IBeam, text_hitbox);
        paint_editor(&data, bounds, window, cx);
        register_mouse_listeners(self.editor.clone(), bounds, window);
    }
}

fn register_mouse_listeners(editor: Entity<WgpuiEditorView>, bounds: Bounds<Pixels>, window: &mut Window) {
    let down = editor.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble
            || event.button != wgpui::MouseButton::Left
            || !bounds.contains(&event.position)
        {
            return;
        }
        let x = (event.position.x - bounds.origin.x).to_f32();
        let y = (event.position.y - bounds.origin.y).to_f32();
        let click_count = event.click_count;
        let shift = event.modifiers.shift;
        down.update(cx, |view, cx| {
            view.mouse_down(x, y, click_count, shift, window, cx);
        });
        cx.stop_propagation();
    });

    let moved = editor.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let x = (event.position.x - bounds.origin.x).to_f32();
        let y = (event.position.y - bounds.origin.y).to_f32();
        let left_down = event.dragging();
        moved.update(cx, |view, cx| view.mouse_move(x, y, left_down, cx));
    });

    window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
        if phase != DispatchPhase::Bubble || event.button != wgpui::MouseButton::Left {
            return;
        }
        let consumed = editor.update(cx, |view, cx| view.mouse_up(cx));
        if consumed {
            cx.stop_propagation();
        }
    });
}

// ---- painting --------------------------------------------------------------

fn hsla(color: SurfaceColor) -> Hsla {
    Rgba {
        r: f32::from(color.red) / 255.0,
        g: f32::from(color.green) / 255.0,
        b: f32::from(color.blue) / 255.0,
        a: f32::from(color.alpha) / 255.0,
    }
    .into()
}

fn with_alpha(color: SurfaceColor, alpha: u8) -> SurfaceColor {
    SurfaceColor { alpha, ..color }
}

fn scaled_alpha(color: SurfaceColor, factor: f32) -> SurfaceColor {
    with_alpha(color, (f32::from(color.alpha) * factor).clamp(0.0, 255.0) as u8)
}

fn rect(origin: Point<Pixels>, x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(origin.x + px(x), origin.y + px(y)),
        size: size(px(width.max(0.0)), px(height.max(0.0))),
    }
}

fn mask(bounds: Bounds<Pixels>) -> Option<ContentMask<Pixels>> {
    Some(ContentMask { bounds })
}

fn decoration_color(theme: &SurfaceTheme, style_id: u32) -> SurfaceColor {
    match style_id {
        SEARCH_MATCH_STYLE_ID => scaled_alpha(theme.selection, 0.8),
        SEARCH_CURRENT_MATCH_STYLE_ID => theme.primary_selection,
        DIAGNOSTIC_ERROR_STYLE_ID => SurfaceColor::rgba(244, 71, 71, 255),
        DIAGNOSTIC_WARNING_STYLE_ID => SurfaceColor::rgba(205, 151, 49, 255),
        DIAGNOSTIC_INFO_STYLE_ID => SurfaceColor::rgba(55, 148, 255, 255),
        DIAGNOSTIC_HINT_STYLE_ID => SurfaceColor::rgba(120, 190, 140, 255),
        _ => theme.decoration,
    }
}

fn is_diagnostic(style_id: u32) -> bool {
    matches!(
        style_id,
        DIAGNOSTIC_ERROR_STYLE_ID
            | DIAGNOSTIC_WARNING_STYLE_ID
            | DIAGNOSTIC_INFO_STYLE_ID
            | DIAGNOSTIC_HINT_STYLE_ID
    )
}

fn paint_text(
    window: &mut Window,
    cx: &mut App,
    row_text: &RowText,
    font: &Font,
    font_size: f32,
    line_height: f32,
    default_color: Hsla,
    origin: Point<Pixels>,
) {
    if row_text.text.is_empty() {
        return;
    }
    let runs: Vec<TextRun> = row_text
        .spans
        .iter()
        .map(|span| {
            let mut run_font = font.clone();
            if span.bold {
                run_font.weight = FontWeight::BOLD;
            }
            if span.italic {
                run_font.style = FontStyle::Italic;
            }
            TextRun {
                len: span.len,
                font: run_font,
                color: span.foreground.map_or(default_color, hsla).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
                letter_spacing: None,
            }
        })
        .collect();
    let line = window.text_system().shape_line(
        SharedString::from(row_text.text.clone()),
        px(font_size),
        &runs,
        None,
    );
    let _ = line.paint(origin, px(line_height), window, cx);
}

fn paint_chevron(window: &mut Window, center: Point<Pixels>, expanded: bool, color: Hsla) {
    let mut builder = PathBuilder::stroke(px(1.5));
    let (half_w, half_h) = (3.5, 2.0);
    if expanded {
        builder.move_to(point(center.x - px(half_w), center.y - px(half_h)));
        builder.line_to(point(center.x, center.y + px(half_h)));
        builder.line_to(point(center.x + px(half_w), center.y - px(half_h)));
    } else {
        builder.move_to(point(center.x - px(half_h), center.y - px(half_w)));
        builder.line_to(point(center.x + px(half_h), center.y));
        builder.line_to(point(center.x - px(half_h), center.y + px(half_w)));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_editor(data: &PaintData, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let origin = bounds.origin;
    let layout = data.layout;
    let theme = &data.frame.theme;
    let lh = data.line_height;
    let cw = data.char_width;
    let foreground = hsla(theme.foreground);
    let hpx = data.first_column as f32 * cw + data.rem_x;
    let code_clip = rect(
        origin,
        layout.gutter,
        0.0,
        layout.code_right - layout.gutter,
        layout.viewport_height,
    );

    // Code region: line backgrounds, guides, selections, decorations, text,
    // fold placeholders and carets, row by row.
    window.with_content_mask(mask(code_clip), |window| {
        for (index, row) in data.frame.rows.iter().enumerate() {
            let y = row.y - data.rem_y;
            if y + lh < 0.0 || y > layout.viewport_height {
                continue;
            }
            let row_color = data.backgrounds[index]
                .or_else(|| row.active.then_some(theme.active_line));
            if let Some(color) = row_color {
                window.paint_quad(fill(
                    rect(origin, layout.gutter, y, layout.code_right - layout.gutter, lh),
                    hsla(color),
                ));
            }
            for guide in &row.indent_guides {
                let x = layout.text_left + *guide as f32 * cw - hpx;
                window.paint_quad(fill(
                    rect(origin, x, y, 1.0, lh),
                    hsla(with_alpha(theme.foreground, 28)),
                ));
            }
            paint_row_overlays(data, row, y, hpx, origin, window);
            let row_text = layout_row_text(
                row,
                data.tab_width,
                data.first_column.saturating_sub(1),
                data.viewport_columns + 4,
            );
            paint_text(
                window,
                cx,
                &row_text,
                &data.font,
                data.font_size,
                lh,
                foreground,
                point(
                    origin.x + px(layout.text_left + row_text.start_column as f32 * cw - hpx),
                    origin.y + px(y),
                ),
            );
            if row.folded {
                let width = chrome::display_width(&row.text, data.tab_width) as f32 * cw;
                let x = layout.text_left + width - hpx + 6.0;
                let chip = rect(origin, x, y + lh * 0.5 - 6.0, 24.0, 12.0);
                window.paint_quad(
                    fill(chip, hsla(with_alpha(theme.foreground, 40))).corner_radii(Corners::all(px(3.0))),
                );
                for dot in 0..3 {
                    window.paint_quad(fill(
                        rect(origin, x + 6.0 + dot as f32 * 5.0, y + lh * 0.5 - 0.5, 2.0, 2.0),
                        hsla(with_alpha(theme.foreground, 170)),
                    ));
                }
            }
            if data.focused {
                for caret in data
                    .frame
                    .carets
                    .iter()
                    .filter(|caret| caret.display_row == row.display_row)
                {
                    window.paint_quad(fill(
                        rect(
                            origin,
                            layout.text_left + caret.x - hpx,
                            y,
                            if caret.primary { 2.0 } else { 1.0 },
                            caret.height,
                        ),
                        hsla(theme.caret),
                    ));
                }
            }
        }
    });

    paint_gutter(data, origin, window);
    paint_gutter_numbers(data, origin, window, cx);
    paint_scrollbars(data, origin, window);
}

fn paint_row_overlays(
    data: &PaintData,
    row: &PaintRow,
    y: f32,
    hpx: f32,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    let layout = data.layout;
    let theme = &data.frame.theme;
    for selection in data
        .frame
        .selections
        .iter()
        .filter(|selection| selection.display_row == row.display_row)
    {
        let color = if selection.primary {
            theme.primary_selection
        } else {
            theme.selection
        };
        window.paint_quad(fill(
            rect(
                origin,
                layout.text_left + selection.x - hpx,
                y,
                selection.width.max(2.0),
                selection.height,
            ),
            hsla(color),
        ));
    }
    for decoration in data
        .frame
        .decoration_geometry
        .iter()
        .filter(|decoration| decoration.display_row == row.display_row)
    {
        let color = decoration_color(theme, decoration.style.id);
        let x = layout.text_left + decoration.x - hpx;
        if is_diagnostic(decoration.style.id) {
            window.paint_quad(fill(
                rect(origin, x, y + decoration.height - 3.0, decoration.width.max(2.0), 1.5),
                hsla(color),
            ));
        } else {
            window.paint_quad(fill(
                rect(origin, x, y, decoration.width.max(2.0), decoration.height),
                hsla(color),
            ));
        }
    }
}

fn paint_gutter(data: &PaintData, origin: Point<Pixels>, window: &mut Window) {
    // Backgrounds, fold chevrons and markers; line numbers need `cx` for text
    // shaping and are painted by `paint_gutter_numbers`.
    let layout = data.layout;
    let theme = &data.frame.theme;
    let lh = data.line_height;
    let gutter_clip = rect(origin, 0.0, 0.0, layout.gutter, layout.viewport_height);
    window.with_content_mask(mask(gutter_clip), |window| {
        window.paint_quad(fill(
            rect(origin, 0.0, 0.0, layout.gutter, layout.viewport_height),
            hsla(theme.gutter_background),
        ));
        window.paint_quad(fill(
            rect(origin, layout.gutter - 1.0, 0.0, 1.0, layout.viewport_height),
            hsla(with_alpha(theme.foreground, 18)),
        ));
        let gutter_hovered = data.hover == Region::Gutter;
        for row in &data.frame.rows {
            let y = row.y - data.rem_y;
            if y + lh < 0.0 || y > layout.viewport_height {
                continue;
            }
            if let Some(gutter) = data
                .frame
                .gutter
                .rows
                .iter()
                .find(|gutter| gutter.display_row == row.display_row)
            {
                let chevron_x = layout.gutter - GUTTER_FOLD_WIDTH / 2.0;
                if gutter.foldable && !row.continuation {
                    let visible = gutter.folded || data.always_show_fold_controls || gutter_hovered;
                    if visible {
                        let alpha = if gutter.folded || gutter_hovered { 230 } else { 120 };
                        paint_chevron(
                            window,
                            point(origin.x + px(chevron_x), origin.y + px(y + lh / 2.0)),
                            !gutter.folded,
                            hsla(with_alpha(theme.gutter_foreground, alpha)),
                        );
                    }
                }
            }
            if let Some(decoration) = data
                .frame
                .decoration_geometry
                .iter()
                .find(|decoration| decoration.display_row == row.display_row)
            {
                let color = decoration_color(theme, decoration.style.id);
                window.paint_quad(
                    fill(
                        rect(origin, GUTTER_MARKER_WIDTH / 2.0 - 3.0, y + lh / 2.0 - 3.0, 6.0, 6.0),
                        hsla(with_alpha(color, 255)),
                    )
                    .corner_radii(Corners::all(px(3.0))),
                );
            }
        }
    });
}

fn paint_gutter_numbers(data: &PaintData, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let layout = data.layout;
    let theme = &data.frame.theme;
    let lh = data.line_height;
    let cw = data.char_width;
    let number_right = GUTTER_MARKER_WIDTH + data.frame.gutter.line_number_width as f32 * cw;
    let clip = rect(origin, 0.0, 0.0, layout.gutter, layout.viewport_height);
    window.with_content_mask(mask(clip), |window| {
        for row in &data.frame.rows {
            let y = row.y - data.rem_y;
            if y + lh < 0.0 || y > layout.viewport_height || row.continuation {
                continue;
            }
            let number = (row.buffer_line + 1).to_string();
            let color = if row.active {
                theme.foreground
            } else {
                theme.gutter_foreground
            };
            let line = RowText {
                text: number.clone(),
                spans: vec![chrome::TextSpan {
                    len: number.len(),
                    foreground: Some(color),
                    bold: false,
                    italic: false,
                }],
                start_column: 0,
            };
            paint_text(
                window,
                cx,
                &line,
                &data.font,
                data.font_size,
                lh,
                hsla(color),
                point(
                    origin.x + px(number_right - number.len() as f32 * cw),
                    origin.y + px(y),
                ),
            );
        }
    });
}

fn minimap_tone_color(theme: &SurfaceTheme, tone: MinimapTone) -> SurfaceColor {
    let syntax = |kind: &str| {
        theme
            .syntax
            .style_for(&mockaco_language::TokenKind::new(kind))
            .map(|style| {
                SurfaceColor::rgba(
                    style.foreground.red,
                    style.foreground.green,
                    style.foreground.blue,
                    255,
                )
            })
    };
    let base = match tone {
        MinimapTone::Text => with_alpha(theme.foreground, 150),
        MinimapTone::Punctuation => with_alpha(theme.foreground, 80),
        MinimapTone::Comment => syntax("comment").unwrap_or(with_alpha(theme.foreground, 90)),
        MinimapTone::String => syntax("string").unwrap_or(with_alpha(theme.foreground, 150)),
        MinimapTone::Number => syntax("number").unwrap_or(with_alpha(theme.foreground, 150)),
    };
    if tone == MinimapTone::Comment {
        scaled_alpha(base, 0.8)
    } else {
        base
    }
}

/// The minimap's lines, for one frame of its buffered layer.
struct MinimapDraw {
    /// Minimap content scroll, in px.
    offset: f32,
    /// How far past the bounds this frame draws (zero for a viewport frame).
    margin: f32,
    rows: Vec<(usize, Vec<MinimapRun>)>,
    theme: SurfaceTheme,
}

/// The minimap's content, drawn into an overscroll-buffered layer.
///
/// The layer's texture covers the minimap plus half its height above and
/// below. Scrolling only shifts that texture; the lines are redrawn once the
/// shift passes half the margin (a quarter of the height), re-centred on the
/// current position, so a refill always lands before the edge of the
/// pre-rendered area comes into view. Nothing ever scrolls past the
/// document's bounds: the margin is only pre-rendered content.
fn minimap_canvas(editor: Entity<WgpuiEditorView>) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| {
            let view = editor.read(cx);
            let layout = view.minimap_layout();
            let offset = layout.content_offset;
            let margin = match prepare_scroll_buffer(window, bounds, point(px(0.), px(-offset))) {
                ScrollBufferFrame::Skip => return None,
                ScrollBufferFrame::Buffer { margin } => margin.height.to_f32(),
                ScrollBufferFrame::Viewport => 0.0,
            };
            let height = bounds.size.height.to_f32();
            let row_count = view.surface.display().row_count();
            let first = ((offset - margin) / MINIMAP_LINE_HEIGHT).floor().max(0.0) as usize;
            let last = ((((offset + height + margin) / MINIMAP_LINE_HEIGHT).ceil() as usize) + 1)
                .min(row_count)
                .max(first);
            Some(MinimapDraw {
                offset,
                margin,
                rows: view.minimap_rows(first..last),
                theme: view.surface.theme(),
            })
        },
        move |bounds, draw, window, _cx| {
            let Some(draw) = draw else {
                return;
            };
            let buffered = draw.margin > 0.0;
            let mask_bounds = if buffered {
                inflate_for_buffer(bounds, size(px(0.), px(draw.margin)))
            } else {
                bounds
            };
            let content_mask = ContentMask {
                bounds: mask_bounds,
            };
            let buffer_mask = buffered.then(|| content_mask.clone());
            window.with_content_mask(Some(content_mask), |window| {
                with_buffer_mask(window, buffer_mask, |window| {
                    for (row, runs) in &draw.rows {
                        let y = *row as f32 * MINIMAP_LINE_HEIGHT - draw.offset;
                        for run in runs {
                            window.paint_quad(fill(
                                rect(
                                    bounds.origin,
                                    MINIMAP_PADDING + run.start as f32 * MINIMAP_CHAR_WIDTH,
                                    y,
                                    run.len as f32 * MINIMAP_CHAR_WIDTH,
                                    MINIMAP_LINE_HEIGHT - 0.5,
                                ),
                                hsla(minimap_tone_color(&draw.theme, run.tone)),
                            ));
                        }
                    }
                });
            });
        },
    )
    .size_full()
}

/// The minimap's edge and its viewport slider. Drawn over the buffered
/// minimap every frame: the slider moves with every scroll, so it must not
/// live in the cached texture.
fn minimap_overlay(editor: Entity<WgpuiEditorView>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            let view = editor.read(cx);
            let layout = view.minimap_layout();
            let theme = view.surface.theme();
            let active =
                matches!(view.drag, Drag::Minimap { .. }) || view.hover == Region::Minimap;
            let origin = bounds.origin;
            let width = bounds.size.width.to_f32();
            window.with_content_mask(mask(bounds), |window| {
                window.paint_quad(fill(
                    rect(origin, 0.0, 0.0, 1.0, bounds.size.height.to_f32()),
                    hsla(with_alpha(theme.foreground, 18)),
                ));
                window.paint_quad(fill(
                    rect(origin, 0.0, layout.slider_top, width, layout.slider_height),
                    hsla(with_alpha(theme.foreground, if active { 38 } else { 22 })),
                ));
            });
        },
    )
    .size_full()
}

fn paint_scrollbars(data: &PaintData, origin: Point<Pixels>, window: &mut Window) {
    let layout = data.layout;
    let theme = &data.frame.theme;
    let thumb_alpha = |region: Region, dragging: bool| -> u8 {
        if dragging {
            150
        } else if data.hover == region {
            110
        } else {
            64
        }
    };
    if let Some(thumb) = data.vthumb {
        let dragging = matches!(data.dragging, Drag::VScroll { .. });
        window.paint_quad(
            fill(
                rect(
                    origin,
                    layout.vbar_left + (VBAR_WIDTH - THUMB_THICKNESS) / 2.0,
                    thumb.start,
                    THUMB_THICKNESS,
                    thumb.length,
                ),
                hsla(with_alpha(theme.foreground, thumb_alpha(Region::VScroll, dragging))),
            )
            .corner_radii(Corners::all(px(THUMB_THICKNESS / 2.0))),
        );
    }
    if let Some(thumb) = data.hthumb {
        let dragging = matches!(data.dragging, Drag::HScroll { .. });
        window.paint_quad(
            fill(
                rect(
                    origin,
                    layout.gutter + thumb.start,
                    layout.viewport_height + (HBAR_HEIGHT - THUMB_THICKNESS) / 2.0,
                    thumb.length,
                    THUMB_THICKNESS,
                ),
                hsla(with_alpha(theme.foreground, thumb_alpha(Region::HScroll, dragging))),
            )
            .corner_radii(Corners::all(px(THUMB_THICKNESS / 2.0))),
        );
    }
}
