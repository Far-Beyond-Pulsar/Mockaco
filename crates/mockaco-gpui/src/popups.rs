//! Popup rendering for language intelligence: the hover card, diagnostic
//! lines, documentation and the completion menu.
//!
//! Documentation is shown through a virtualized [`list`], and the completion
//! menu through a [`uniform_list`], so a thousand items or a long doc page cost
//! the same as a handful: only the rows in view are built and laid out.
//!
//! Building a [`HoverCard`] does all the parsing and highlighting up front;
//! rendering it afterwards is only element construction.

use crate::intel::{fuzzy_positions, CompletionEntry, CompletionKind, CompletionSession};
use crate::markdown::{Block, Span};
use crate::{Language, SurfaceColor, SurfaceTheme};
use mockaco_core::{DiagnosticSeverity, Document, Text};
use mockaco_language::{GenericSyntaxProvider, RustTreeSitterProvider, SyntaxHighlightProvider};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use wgpui::prelude::FluentBuilder as _;
use wgpui::{
    div, font, list, px, uniform_list, AnyElement, App, ElementId, FontFallbacks, FontStyle,
    FontWeight, Hsla, InteractiveElement, InteractiveText, IntoElement, ListAlignment, ListState,
    ParentElement, Pixels, Rgba, ScrollStrategy, SharedString, StatefulInteractiveElement, Styled,
    StyledText, TextRun, UnderlineStyle, UniformListScrollHandle, Window,
};

/// Width of the hover card and the documentation panel.
pub(crate) const CARD_WIDTH: f32 = 520.0;
/// Tallest a hover card grows before its list scrolls.
pub(crate) const CARD_MAX_HEIGHT: f32 = 360.0;
pub(crate) const MENU_WIDTH: f32 = 440.0;
pub(crate) const MENU_ROW_HEIGHT: f32 = 22.0;
pub(crate) const MENU_MAX_ROWS: usize = 10;
const DOCS_MAX_HEIGHT: f32 = 300.0;
/// Code blocks longer than this are shown unhighlighted.
const MAX_HIGHLIGHT_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct PopupColors {
    pub background: Hsla,
    pub border: Hsla,
    pub foreground: Hsla,
    pub muted: Hsla,
    pub link: Hsla,
    pub code_background: Hsla,
    pub selected: Hsla,
    pub accent: Hsla,
    pub error: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    pub hint: Hsla,
}

fn hsla(color: SurfaceColor) -> Hsla {
    Rgba {
        r: f32::from(color.red) / 255.0,
        g: f32::from(color.green) / 255.0,
        b: f32::from(color.blue) / 255.0,
        a: f32::from(color.alpha) / 255.0,
    }
    .into()
}

fn blend(base: SurfaceColor, over: SurfaceColor, amount: f32) -> SurfaceColor {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    SurfaceColor::rgba(
        mix(base.red, over.red),
        mix(base.green, over.green),
        mix(base.blue, over.blue),
        255,
    )
}

impl PopupColors {
    pub fn from_theme(theme: &SurfaceTheme) -> Self {
        let background = blend(theme.background, theme.foreground, 0.07);
        Self {
            background: hsla(background),
            border: hsla(blend(theme.background, theme.foreground, 0.22)),
            foreground: hsla(theme.foreground),
            muted: hsla(blend(background, theme.foreground, 0.55)),
            link: hsla(SurfaceColor::rgba(86, 156, 255, 255)),
            code_background: hsla(blend(theme.background, theme.foreground, 0.11)),
            selected: hsla(blend(background, theme.selection, 0.85)),
            accent: hsla(SurfaceColor::rgba(255, 196, 92, 255)),
            error: hsla(SurfaceColor::rgba(244, 71, 71, 255)),
            warning: hsla(SurfaceColor::rgba(205, 151, 49, 255)),
            info: hsla(SurfaceColor::rgba(55, 148, 255, 255)),
            hint: hsla(SurfaceColor::rgba(120, 190, 140, 255)),
        }
    }

    pub fn severity(&self, severity: DiagnosticSeverity) -> Hsla {
        match severity {
            DiagnosticSeverity::Error => self.error,
            DiagnosticSeverity::Warning => self.warning,
            DiagnosticSeverity::Info => self.info,
            DiagnosticSeverity::Hint => self.hint,
        }
    }
}

// ---- prepared blocks ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextKind {
    Paragraph,
    Heading(u8),
    Quote,
    ListItem { depth: usize },
}

#[derive(Clone)]
pub(crate) struct TextRunStyle {
    len: usize,
    bold: bool,
    italic: bool,
    code: bool,
    link: bool,
}

#[derive(Clone)]
pub(crate) struct CodeRun {
    len: usize,
    color: Option<Hsla>,
    bold: bool,
    italic: bool,
}

/// A markdown block with its text flattened and its code highlighted.
#[derive(Clone)]
pub(crate) enum BlockView {
    Text {
        kind: TextKind,
        marker: Option<SharedString>,
        text: SharedString,
        runs: Vec<TextRunStyle>,
        links: Vec<(Range<usize>, String)>,
    },
    Code {
        text: SharedString,
        runs: Vec<CodeRun>,
    },
    Rule,
}

fn flatten(spans: &[Span]) -> (SharedString, Vec<TextRunStyle>, Vec<(Range<usize>, String)>) {
    let mut text = String::new();
    let mut runs = Vec::with_capacity(spans.len());
    let mut links = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        runs.push(TextRunStyle {
            len: span.text.len(),
            bold: span.style.bold,
            italic: span.style.italic,
            code: span.style.code,
            link: span.style.link.is_some(),
        });
        if let Some(target) = &span.style.link {
            links.push((start..text.len(), target.clone()));
        }
    }
    (text.into(), runs, links)
}

/// The language a fenced block is written in.
fn language_for_fence(info: &str, fallback: Language) -> Language {
    match info {
        "" | "text" | "txt" | "plain" | "plaintext" => {
            if info.is_empty() {
                fallback
            } else {
                Language::Plain
            }
        }
        "rust" | "rs" => Language::Rust,
        "python" | "py" => Language::from_extension("py"),
        "javascript" | "js" => Language::from_extension("js"),
        "typescript" | "ts" => Language::from_extension("ts"),
        "json" => Language::from_extension("json"),
        "toml" => Language::from_extension("toml"),
        "yaml" | "yml" => Language::from_extension("yaml"),
        "c" => Language::from_extension("c"),
        "cpp" | "c++" => Language::from_extension("cpp"),
        "sh" | "bash" | "shell" => Language::from_extension("sh"),
        other => Language::from_extension(other),
    }
}

static HIGHLIGHT_VERSION: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static RUST_PROVIDER: Option<RustTreeSitterProvider> = RustTreeSitterProvider::new().ok();
}

fn highlight_code(language: Language, code: &str, theme: &SurfaceTheme) -> Vec<CodeRun> {
    let plain = || {
        vec![CodeRun {
            len: code.len(),
            color: None,
            bold: false,
            italic: false,
        }]
    };
    if code.is_empty() || code.len() > MAX_HIGHLIGHT_BYTES {
        return plain();
    }
    // Providers cache their parse by document version; each block needs a
    // version no other block has used.
    let version = HIGHLIGHT_VERSION.fetch_add(1, Ordering::Relaxed);
    let snapshot = Document::snapshot_with_version(Text::new(code), version);
    let result = match language {
        Language::Rust => RUST_PROVIDER.with(|provider| {
            provider
                .as_ref()
                .and_then(|p| p.highlight(&snapshot, 0..code.len()).ok())
        }),
        Language::Syntax(syntax) => GenericSyntaxProvider::new(syntax)
            .highlight(&snapshot, 0..code.len())
            .ok(),
        Language::Plain => None,
    };
    let Some(result) = result else {
        return plain();
    };

    // Innermost (shortest) token wins per byte: paint longest first.
    let mut tokens: Vec<_> = result.tokens.iter().collect();
    tokens.sort_by_key(|token| std::cmp::Reverse(token.range.len()));
    let mut styles: Vec<Option<usize>> = vec![None; code.len()];
    for (index, token) in tokens.iter().enumerate() {
        let end = token.range.end.min(code.len());
        for slot in styles.iter_mut().take(end).skip(token.range.start.min(end)) {
            *slot = Some(index);
        }
    }

    let mut runs: Vec<CodeRun> = Vec::new();
    let mut byte = 0;
    for character in code.chars() {
        let width = character.len_utf8();
        let style = styles[byte].and_then(|index| theme.syntax.style_for(&tokens[index].kind));
        let (color, bold, italic) = match style {
            Some(style) => (
                Some(hsla(SurfaceColor::rgba(
                    style.foreground.red,
                    style.foreground.green,
                    style.foreground.blue,
                    style.foreground.alpha,
                ))),
                style.font.bold,
                style.font.italic,
            ),
            None => (None, false, false),
        };
        match runs.last_mut() {
            Some(last) if last.color == color && last.bold == bold && last.italic == italic => {
                last.len += width
            }
            _ => runs.push(CodeRun {
                len: width,
                color,
                bold,
                italic,
            }),
        }
        byte += width;
    }
    runs
}

pub(crate) fn build_blocks(
    blocks: &[Block],
    default_language: Language,
    theme: &SurfaceTheme,
) -> Vec<BlockView> {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(spans) => {
                let (text, runs, links) = flatten(spans);
                BlockView::Text {
                    kind: TextKind::Paragraph,
                    marker: None,
                    text,
                    runs,
                    links,
                }
            }
            Block::Heading { level, spans } => {
                let (text, mut runs, links) = flatten(spans);
                for run in &mut runs {
                    run.bold = true;
                }
                BlockView::Text {
                    kind: TextKind::Heading(*level),
                    marker: None,
                    text,
                    runs,
                    links,
                }
            }
            Block::Quote(spans) => {
                let (text, runs, links) = flatten(spans);
                BlockView::Text {
                    kind: TextKind::Quote,
                    marker: None,
                    text,
                    runs,
                    links,
                }
            }
            Block::ListItem {
                depth,
                marker,
                spans,
            } => {
                let (text, runs, links) = flatten(spans);
                BlockView::Text {
                    kind: TextKind::ListItem { depth: *depth },
                    marker: Some(marker.clone().into()),
                    text,
                    runs,
                    links,
                }
            }
            Block::Code { language, text } => {
                let language = language_for_fence(language, default_language);
                BlockView::Code {
                    runs: highlight_code(language, text, theme),
                    text: text.clone().into(),
                }
            }
            Block::Rule => BlockView::Rule,
        })
        .collect()
}

// ---- hover card --------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct DiagnosticLine {
    pub severity: DiagnosticSeverity,
    pub message: SharedString,
    pub source: Option<SharedString>,
}

/// Everything a hover popup shows, prepared once.
#[derive(Clone)]
pub(crate) struct HoverCard {
    pub diagnostics: Vec<DiagnosticLine>,
    pub blocks: Rc<Vec<BlockView>>,
    pub list: ListState,
}

impl HoverCard {
    pub fn new(diagnostics: Vec<DiagnosticLine>, blocks: Vec<BlockView>) -> Self {
        Self {
            diagnostics,
            list: ListState::new(blocks.len(), ListAlignment::Top, px(800.0)),
            blocks: Rc::new(blocks),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty() && self.blocks.is_empty()
    }
}

fn mono_font(family: &SharedString) -> wgpui::Font {
    let mut mono = font(family.clone());
    mono.fallbacks = Some(FontFallbacks::from_fonts(vec![
        "Cascadia Mono".to_owned(),
        "Consolas".to_owned(),
        "Menlo".to_owned(),
        "DejaVu Sans Mono".to_owned(),
        "monospace".to_owned(),
    ]));
    mono
}

fn text_runs(
    runs: &[TextRunStyle],
    colors: &PopupColors,
    mono: &SharedString,
    window: &Window,
) -> Vec<TextRun> {
    let base = window.text_style().font();
    runs.iter()
        .filter(|run| run.len > 0)
        .map(|run| {
            let mut run_font = if run.code {
                mono_font(mono)
            } else {
                base.clone()
            };
            if run.bold {
                run_font.weight = FontWeight::BOLD;
            }
            if run.italic {
                run_font.style = FontStyle::Italic;
            }
            let color = if run.link {
                colors.link
            } else {
                colors.foreground
            };
            TextRun {
                len: run.len,
                font: run_font,
                color: color.into(),
                background_color: run.code.then_some(colors.code_background),
                underline: run.link.then(|| UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(colors.link),
                    wavy: false,
                }),
                strikethrough: None,
                letter_spacing: None,
            }
        })
        .collect()
}

fn render_block(
    block: &BlockView,
    index: usize,
    colors: &PopupColors,
    mono: &SharedString,
    size: f32,
    window: &Window,
) -> AnyElement {
    match block {
        BlockView::Rule => div()
            .w_full()
            .h(px(1.0))
            .my(px(6.0))
            .bg(colors.border)
            .into_any_element(),
        BlockView::Code { text, runs } => {
            let base = mono_font(mono);
            let runs: Vec<TextRun> = runs
                .iter()
                .map(|run| {
                    let mut run_font = base.clone();
                    if run.bold {
                        run_font.weight = FontWeight::BOLD;
                    }
                    if run.italic {
                        run_font.style = FontStyle::Italic;
                    }
                    TextRun {
                        len: run.len,
                        font: run_font,
                        color: run.color.unwrap_or(colors.foreground).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                        letter_spacing: None,
                    }
                })
                .collect();
            div()
                .w_full()
                .my(px(3.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(4.0))
                .bg(colors.code_background)
                .text_size(px(size - 1.0))
                .child(StyledText::new(text.clone()).with_runs(runs))
                .into_any_element()
        }
        BlockView::Text {
            kind,
            marker,
            text,
            runs,
            links,
        } => {
            let styled =
                StyledText::new(text.clone()).with_runs(text_runs(runs, colors, mono, window));
            let body: AnyElement = if links.is_empty() {
                styled.into_any_element()
            } else {
                let ranges: Vec<Range<usize>> =
                    links.iter().map(|(range, _)| range.clone()).collect();
                let targets: Vec<String> = links.iter().map(|(_, target)| target.clone()).collect();
                InteractiveText::new(ElementId::from(("doc-link", index)), styled)
                    .on_click(ranges, move |link, _window, cx| {
                        if let Some(target) = targets.get(link) {
                            if target.starts_with("http://") || target.starts_with("https://") {
                                cx.open_url(target);
                            }
                        }
                    })
                    .into_any_element()
            };
            let text_size = match kind {
                TextKind::Heading(1) => size + 4.0,
                TextKind::Heading(2) => size + 2.0,
                TextKind::Heading(_) => size + 1.0,
                _ => size,
            };
            let row = div()
                .w_full()
                .py(px(2.0))
                .text_size(px(text_size))
                .text_color(colors.foreground);
            match kind {
                TextKind::Quote => row
                    .pl(px(8.0))
                    .border_l_2()
                    .border_color(colors.border)
                    .text_color(colors.muted)
                    .child(body)
                    .into_any_element(),
                TextKind::ListItem { depth } => row
                    .pl(px(8.0 + *depth as f32 * 14.0))
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_none()
                            .text_color(colors.muted)
                            .child(marker.clone().unwrap_or_default()),
                    )
                    .child(div().flex_1().min_w_0().child(body))
                    .into_any_element(),
                _ => row.child(body).into_any_element(),
            }
        }
    }
}

/// The documentation blocks of `card` as a virtualized list that sizes to its
/// content up to `max_height`.
fn blocks_list(
    card: &HoverCard,
    colors: PopupColors,
    mono: SharedString,
    size: f32,
    max_height: f32,
) -> AnyElement {
    let blocks = card.blocks.clone();
    list(card.list.clone(), move |index, window, _cx| {
        match blocks.get(index) {
            Some(block) => render_block(block, index, &colors, &mono, size, window),
            None => div().into_any_element(),
        }
    })
    .with_sizing_behavior(wgpui::ListSizingBehavior::Infer)
    .w_full()
    .max_h(px(max_height))
    .into_any_element()
}

fn card_shell(colors: &PopupColors, width: f32) -> wgpui::Div {
    div()
        .flex()
        .flex_col()
        .w(px(width))
        .max_w(px(width))
        .px(px(10.0))
        .py(px(6.0))
        .bg(colors.background)
        .border_1()
        .border_color(colors.border)
        .rounded(px(6.0))
        .shadow_lg()
        .occlude()
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
}

pub(crate) fn hover_card(
    card: &HoverCard,
    colors: PopupColors,
    mono: SharedString,
    size: f32,
    on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let mut shell = card_shell(&colors, CARD_WIDTH)
        .id("mockaco-hover-card")
        .on_hover(on_hover)
        .text_size(px(size));
    for line in &card.diagnostics {
        shell = shell.child(
            div()
                .w_full()
                .py(px(2.0))
                .flex()
                .flex_row()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_none()
                        .mt(px(5.0))
                        .size(px(8.0))
                        .rounded_full()
                        .bg(colors.severity(line.severity)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(colors.foreground)
                        .child(line.message.clone())
                        .when_some(line.source.clone(), |element, source| {
                            element.child(div().text_color(colors.muted).child(source))
                        }),
                ),
        );
    }
    if !card.diagnostics.is_empty() && !card.blocks.is_empty() {
        shell = shell.child(div().w_full().h(px(1.0)).my(px(4.0)).bg(colors.border));
    }
    if !card.blocks.is_empty() {
        shell = shell.child(blocks_list(card, colors, mono, size, CARD_MAX_HEIGHT));
    }
    shell.into_any_element()
}

// ---- completion menu ---------------------------------------------------------

/// The documentation panel beside the completion menu for one entry.
#[derive(Clone)]
pub(crate) struct DocsPanel {
    pub card: HoverCard,
}

fn kind_color(kind: CompletionKind, colors: &PopupColors) -> Hsla {
    match kind {
        CompletionKind::Method | CompletionKind::Function | CompletionKind::Constructor => {
            colors.info
        }
        CompletionKind::Field | CompletionKind::Property | CompletionKind::Variable => colors.hint,
        CompletionKind::Class
        | CompletionKind::Struct
        | CompletionKind::Enum
        | CompletionKind::Interface
        | CompletionKind::TypeParameter => colors.accent,
        CompletionKind::Keyword | CompletionKind::Operator => colors.link,
        _ => colors.muted,
    }
}

fn menu_row(
    rank: usize,
    session: &CompletionSession,
    colors: &PopupColors,
    on_pick: &Rc<dyn Fn(usize, &mut Window, &mut App)>,
) -> AnyElement {
    let Some(entry) = session.entry(rank) else {
        return div().into_any_element();
    };
    let selected = rank == session.selected();
    let matched = fuzzy_positions(
        session.prefix(),
        entry.filter_text.as_deref().unwrap_or(&entry.label),
    );
    // Highlight matches only when the label is what was matched.
    let label_matched =
        entry.filter_text.is_none() || entry.filter_text.as_deref() == Some(&entry.label);

    let mut label = div()
        .flex()
        .flex_row()
        .flex_none()
        .text_color(colors.foreground);
    if entry.deprecated {
        label = label.line_through();
    }
    // Group consecutive characters by matched/unmatched to keep the element
    // count small.
    let mut group = String::new();
    let mut group_matched = false;
    for (index, character) in entry.label.chars().enumerate() {
        let is_match = label_matched && matched.contains(&index);
        if is_match != group_matched && !group.is_empty() {
            label = label.child(label_part(
                std::mem::take(&mut group),
                group_matched,
                colors,
            ));
        }
        group_matched = is_match;
        group.push(character);
    }
    if !group.is_empty() {
        label = label.child(label_part(group, group_matched, colors));
    }

    let on_pick = on_pick.clone();
    div()
        .id(ElementId::from(("completion-row", rank)))
        .w_full()
        .h(px(MENU_ROW_HEIGHT))
        .px(px(8.0))
        .gap(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .when(selected, |row| row.bg(colors.selected))
        .on_mouse_down(wgpui::MouseButton::Left, move |_, window, cx| {
            on_pick(rank, window, cx);
            cx.stop_propagation();
        })
        .child(
            div()
                .flex_none()
                .w(px(14.0))
                .text_color(kind_color(entry.kind, colors))
                .child(entry.kind.glyph()),
        )
        .child(label)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(colors.muted)
                .child(
                    entry
                        .label_detail
                        .clone()
                        .or_else(|| entry.detail.clone())
                        .unwrap_or_default(),
                ),
        )
        .into_any_element()
}

fn label_part(text: String, matched: bool, colors: &PopupColors) -> AnyElement {
    let part = div().child(text);
    if matched {
        part.text_color(colors.accent)
            .font_weight(FontWeight::BOLD)
            .into_any_element()
    } else {
        part.into_any_element()
    }
}

/// The completion menu: a virtualized list of ranked entries, optionally with
/// the selected entry's documentation to its right.
pub(crate) fn completion_menu(
    session: &Rc<RefCell<CompletionSession>>,
    scroll: &UniformListScrollHandle,
    docs: Option<&DocsPanel>,
    colors: PopupColors,
    mono: SharedString,
    size: f32,
    on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
) -> AnyElement {
    let count = session.borrow().len();
    let rows = count.min(MENU_MAX_ROWS);
    let height = rows as f32 * MENU_ROW_HEIGHT;

    let list_session = session.clone();
    let list_on_pick = on_pick.clone();
    let rows_list = uniform_list(
        "mockaco-completion-list",
        count,
        move |range, _window, _cx| {
            let session = list_session.borrow();
            range
                .map(|rank| menu_row(rank, &session, &colors, &list_on_pick))
                .collect::<Vec<_>>()
        },
    )
    .track_scroll(scroll)
    .w_full()
    .h(px(height));

    let footer = {
        let session = session.borrow();
        session
            .selected_entry()
            .map(|entry| (entry.detail.clone(), session.selected() + 1, session.len()))
    };

    let menu = div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(MENU_WIDTH))
        .bg(colors.background)
        .border_1()
        .border_color(colors.border)
        .rounded(px(6.0))
        .shadow_lg()
        .occlude()
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .overflow_hidden()
        .text_size(px(size))
        .child(rows_list)
        .when_some(footer, |menu, (detail, selected, total)| {
            menu.child(
                div()
                    .w_full()
                    .px(px(8.0))
                    .py(px(3.0))
                    .border_t_1()
                    .border_color(colors.border)
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .text_size(px(size - 1.0))
                    .text_color(colors.muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(detail.unwrap_or_default()),
                    )
                    .child(div().flex_none().child(format!("{selected}/{total}"))),
            )
        });

    let mut row = div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(4.0))
        .child(menu);
    if let Some(docs) = docs {
        if !docs.card.is_empty() {
            row = row.child(
                card_shell(&colors, CARD_WIDTH * 0.8)
                    .text_size(px(size))
                    .child(blocks_list(&docs.card, colors, mono, size, DOCS_MAX_HEIGHT)),
            );
        }
    }
    row.into_any_element()
}

/// Keeps `rank` visible in the menu.
pub(crate) fn reveal(scroll: &UniformListScrollHandle, rank: usize) {
    scroll.scroll_to_item(rank, ScrollStrategy::Nearest);
}

/// Pixel height of the menu for `count` entries (for placement decisions).
pub(crate) fn menu_height(count: usize) -> Pixels {
    px(count.min(MENU_MAX_ROWS) as f32 * MENU_ROW_HEIGHT + 26.0)
}

pub(crate) fn entry_doc_blocks(entry: &CompletionEntry) -> String {
    let mut markdown = String::new();
    if let Some(detail) = &entry.detail {
        markdown.push_str("```rust\n");
        markdown.push_str(detail);
        markdown.push_str("\n```\n\n");
    }
    if let Some(documentation) = &entry.documentation {
        markdown.push_str(documentation);
    }
    markdown
}
