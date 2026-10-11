//! Language intelligence for [`WgpuiEditorView`]: diagnostics, hover,
//! completion and go-to-definition.
//!
//! The view never talks to a language server. It decides *when* to ask
//! (a pointer resting on a word, an identifier being typed, Ctrl+Space,
//! Ctrl+click) by emitting an [`EditorEvent`] carrying a [`LspRequest`], and
//! the host answers through [`WgpuiEditorView::show_hover`] and friends. Every
//! answer names the request and document version it belongs to; one that no
//! longer matches (the user typed or moved on) is dropped, so a slow server
//! can never paint over newer state.
//!
//! Nothing here runs per frame except placing the open popups: hover and
//! completion work happens on pointer rest and on edits, and the popups
//! themselves are virtualized (see [`crate::popups`]).

use super::{EditorEvent, InputEvent, Key, LspRequest, Region, WgpuiEditorView};
use crate::intel::{
    self, CompletionEntry, CompletionSession, DiagnosticInput, HoverContent, PositionEdit,
};
use crate::markdown;
use crate::popups::{self, DiagnosticLine, DocsPanel, HoverCard, PopupColors};
use mockaco_core::{Edit, Grouping, Selection, SelectionSet, Transaction};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;
use wgpui::{
    anchored, deferred, point, px, AnyElement, App, Context, Corner, IntoElement, ParentElement,
    Pixels, Point, SharedString, Task, UniformListScrollHandle, Window,
};

/// How long the pointer must rest on a word before hover is requested.
const HOVER_DELAY: Duration = Duration::from_millis(350);
/// Grace period between the pointer leaving a word and the card closing, so
/// the pointer can travel onto the card.
const HOVER_HIDE_DELAY: Duration = Duration::from_millis(250);
/// Typing pause before completion is requested.
const COMPLETION_DELAY: Duration = Duration::from_millis(60);
/// Selection rest before an entry's documentation is fetched.
const RESOLVE_DELAY: Duration = Duration::from_millis(150);
const POPUP_TEXT_SIZE: f32 = 13.0;

pub(super) struct Intel {
    pub enabled: bool,
    /// Window position of the editor's top-left, recorded while painting.
    pub origin: Point<Pixels>,
    next_request: u64,

    hover_pending: Option<(Range<usize>, usize)>,
    hover_word: Option<Range<usize>>,
    hover_request: u64,
    hover_card: Option<HoverCard>,
    hover_task: Option<Task<()>>,
    hover_hide_task: Option<Task<()>>,
    card_hovered: bool,

    completion: Option<Rc<RefCell<CompletionSession>>>,
    completion_scroll: UniformListScrollHandle,
    completion_docs: Option<DocsPanel>,
    completion_request: u64,
    completion_anchor: usize,
    completion_caret: usize,
    completion_task: Option<Task<()>>,
    resolve_task: Option<Task<()>>,
    resolved: HashSet<u64>,
}

impl Default for Intel {
    fn default() -> Self {
        Self {
            enabled: false,
            origin: Point::default(),
            next_request: 0,
            hover_pending: None,
            hover_word: None,
            hover_request: 0,
            hover_card: None,
            hover_task: None,
            hover_hide_task: None,
            card_hovered: false,
            completion: None,
            completion_scroll: UniformListScrollHandle::new(),
            completion_docs: None,
            completion_request: 0,
            completion_anchor: 0,
            completion_caret: 0,
            completion_task: None,
            resolve_task: None,
            resolved: HashSet::new(),
        }
    }
}

/// What an input event did to the text, as far as completion cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditKind {
    Typed(char),
    Deleted,
    Other,
}

impl EditKind {
    pub fn of(event: &InputEvent) -> Self {
        match event {
            InputEvent::Text(text) => {
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Self::Typed(c),
                    _ => Self::Other,
                }
            }
            InputEvent::Key(key) if matches!(key.key, Key::Backspace | Key::Delete) => {
                Self::Deleted
            }
            _ => Self::Other,
        }
    }
}

impl WgpuiEditorView {
    // ---- host API ---------------------------------------------------------

    /// Turns hover/completion/definition requests on or off. Off by default,
    /// so a plain editor never emits LSP events.
    pub fn set_language_intelligence(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.intel.enabled = enabled;
        if !enabled {
            self.dismiss_hover(cx);
            self.close_completion(cx);
        }
    }

    /// Version of the document right now; answers carry the version they
    /// were computed for.
    pub fn document_version(&self) -> u64 {
        self.surface.document().version()
    }

    /// What a language server must be told to catch up with the edits made
    /// since this was last called. Cheap; never copies the document.
    pub fn take_pending_sync(&mut self) -> crate::PendingSync {
        self.surface.take_pending_sync()
    }

    /// Replaces the diagnostics. Underlines, gutter markers and the hover
    /// card are all driven by this one call.
    pub fn set_diagnostics(&mut self, diagnostics: Vec<DiagnosticInput>, cx: &mut Context<Self>) {
        let converted: Vec<mockaco_core::Diagnostic> = {
            let text = self.surface.document().text();
            diagnostics
                .into_iter()
                .filter_map(|input| {
                    let start = text.utf16_position_to_byte(input.start).ok()?;
                    let end = text.utf16_position_to_byte(input.end).unwrap_or(start);
                    let mut diagnostic = mockaco_core::Diagnostic::new(
                        start..end.max(start),
                        input.severity,
                        input.message,
                    );
                    if let Some(source) = input.source {
                        diagnostic = diagnostic.source(source);
                    }
                    Some(diagnostic)
                })
                .collect()
        };
        self.surface.set_lsp_diagnostics(converted);
        cx.notify();
    }

    /// Answers a [`EditorEvent::HoverRequested`].
    pub fn show_hover(
        &mut self,
        request: u64,
        version: u64,
        content: Option<HoverContent>,
        cx: &mut Context<Self>,
    ) {
        if request != self.intel.hover_request
            || version != self.document_version()
            || self.intel.hover_word.is_none()
        {
            return;
        }
        let diagnostics = self
            .intel
            .hover_card
            .as_ref()
            .map(|card| card.diagnostics.clone())
            .unwrap_or_default();
        let blocks = match content.filter(|content| !content.is_empty()) {
            Some(content) => popups::build_blocks(
                &content.blocks,
                self.surface.language(),
                &self.surface.theme(),
            ),
            None => Vec::new(),
        };
        let card = HoverCard::new(diagnostics, blocks);
        self.intel.hover_card = (!card.is_empty()).then_some(card);
        cx.notify();
    }

    /// Answers a [`EditorEvent::CompletionRequested`].
    pub fn show_completions(
        &mut self,
        request: u64,
        version: u64,
        items: Vec<CompletionEntry>,
        cx: &mut Context<Self>,
    ) {
        if request != self.intel.completion_request || version != self.document_version() {
            return;
        }
        let Some(caret) = self.primary_caret() else {
            return;
        };
        if caret != self.intel.completion_caret {
            return;
        }
        let anchor = self.intel.completion_anchor;
        if items.is_empty() || anchor > caret {
            self.close_completion(cx);
            return;
        }
        let prefix = self
            .surface
            .document()
            .text()
            .slice(anchor..caret)
            .into_owned();
        let session = CompletionSession::new(request, anchor, items, &prefix);
        if session.is_empty() {
            self.close_completion(cx);
            return;
        }
        self.intel.completion = Some(Rc::new(RefCell::new(session)));
        self.intel.completion_scroll = UniformListScrollHandle::new();
        self.intel.resolved.clear();
        self.refresh_docs(cx);
        cx.notify();
    }

    /// Answers a [`EditorEvent::CompletionResolveRequested`].
    pub fn resolve_completion(
        &mut self,
        request: u64,
        host_data: u64,
        documentation: Option<String>,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.intel.completion.clone() else {
            return;
        };
        if session.borrow().request != request {
            return;
        }
        self.intel.resolved.insert(host_data);
        let known = session
            .borrow_mut()
            .resolve(host_data, documentation, detail);
        let selected = session
            .borrow()
            .selected_entry()
            .map(|entry| entry.host_data);
        if known && selected == Some(host_data) {
            self.refresh_docs(cx);
            cx.notify();
        }
    }

    /// Applies edits expressed in LSP coordinates to the document as it is
    /// now (late-resolved completion edits such as an added `use`). They join
    /// the previous undo step.
    pub fn apply_position_edits(&mut self, edits: Vec<PositionEdit>, cx: &mut Context<Self>) {
        if self.read_only || edits.is_empty() {
            return;
        }
        let converted: Vec<Edit> = {
            let text = self.surface.document().text();
            edits
                .iter()
                .filter_map(|edit| {
                    let start = text.utf16_position_to_byte(edit.start).ok()?;
                    let end = text.utf16_position_to_byte(edit.end).ok()?;
                    Some(Edit::replace(start..end.max(start), edit.new_text.clone()))
                })
                .collect()
        };
        if converted.is_empty() {
            return;
        }
        let transaction = Transaction::from_edits(converted);
        if self
            .surface
            .apply_transaction(&transaction, Grouping::CurrentGroup)
            .is_ok()
        {
            cx.emit(EditorEvent::Changed);
            cx.notify();
        }
    }

    /// Opens completion at the caret (Ctrl+Space).
    pub fn trigger_completion(&mut self, cx: &mut Context<Self>) {
        if self.intel.enabled && !self.read_only {
            self.request_completion(None, cx);
        }
    }

    /// Asks the host for the definition of the symbol at the caret (F12).
    pub fn request_definition_at_caret(&mut self, cx: &mut Context<Self>) {
        if !self.intel.enabled {
            return;
        }
        if let Some(caret) = self.primary_caret() {
            self.emit_definition(caret, cx);
        }
    }

    // ---- shared helpers ---------------------------------------------------

    fn primary_caret(&self) -> Option<usize> {
        let selections = self.surface.editor().selections();
        if selections.len() != 1 {
            return None;
        }
        selections
            .primary()
            .filter(|selection| selection.is_caret())
            .map(|selection| selection.cursor())
    }

    fn lsp_request(&mut self, offset: usize) -> Option<LspRequest> {
        let position = self
            .surface
            .document()
            .text()
            .byte_to_utf16_position(offset)
            .ok()?;
        self.intel.next_request += 1;
        Some(LspRequest {
            id: self.intel.next_request,
            version: self.document_version(),
            offset,
            position,
        })
    }

    fn emit_definition(&mut self, offset: usize, cx: &mut Context<Self>) {
        if let Some(request) = self.lsp_request(offset) {
            cx.emit(EditorEvent::DefinitionRequested(request));
        }
    }

    // ---- hover ------------------------------------------------------------

    /// Follows the pointer: starts the dwell timer on a new word, keeps the
    /// card on its word, and closes it (after a grace period) elsewhere.
    pub(super) fn update_hover(&mut self, x: f32, y: f32, region: Region, cx: &mut Context<Self>) {
        // While the menu is open (or the pointer is on a card) hover stays put.
        if !self.intel.enabled || self.intel.card_hovered || self.intel.completion.is_some() {
            return;
        }
        let target = if region == Region::Text && y < self.layout.viewport_height {
            let (rem_x, rem_y) = self.remainders();
            let text = self.surface.document().text();
            self.surface
                .byte_under_pointer(x + rem_x, y + rem_y)
                .and_then(|byte| intel::word_range(text, byte).map(|word| (word, byte)))
        } else {
            None
        };
        match target {
            Some((word, byte)) => {
                let on_shown = self.intel.hover_word.as_ref() == Some(&word)
                    && self.intel.hover_card.is_some();
                let on_pending = self
                    .intel
                    .hover_pending
                    .as_ref()
                    .is_some_and(|(pending, _)| *pending == word);
                if on_shown {
                    self.intel.hover_hide_task = None;
                    return;
                }
                if on_pending {
                    return;
                }
                self.schedule_hover_hide(cx);
                self.intel.hover_pending = Some((word, byte));
                self.intel.hover_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(HOVER_DELAY).await;
                    let _ = this.update(cx, |view, cx| view.fire_hover(cx));
                }));
            }
            None => {
                self.intel.hover_pending = None;
                self.intel.hover_task = None;
                self.schedule_hover_hide(cx);
            }
        }
    }

    fn fire_hover(&mut self, cx: &mut Context<Self>) {
        let Some((word, byte)) = self.intel.hover_pending.take() else {
            return;
        };
        self.intel.hover_task = None;
        self.intel.hover_hide_task = None;
        let diagnostics: Vec<DiagnosticLine> = self
            .surface
            .diagnostics_at(byte)
            .into_iter()
            .map(|diagnostic| DiagnosticLine {
                severity: diagnostic.severity,
                message: diagnostic.message.clone().into(),
                source: diagnostic.source.clone().map(SharedString::from),
            })
            .collect();
        self.intel.hover_word = Some(word);
        self.intel.hover_card =
            (!diagnostics.is_empty()).then(|| HoverCard::new(diagnostics, Vec::new()));
        match self.lsp_request(byte) {
            Some(request) => {
                self.intel.hover_request = request.id;
                cx.emit(EditorEvent::HoverRequested(request));
            }
            None => self.intel.hover_request = 0,
        }
        cx.notify();
    }

    fn schedule_hover_hide(&mut self, cx: &mut Context<Self>) {
        if self.intel.hover_card.is_none() || self.intel.hover_hide_task.is_some() {
            return;
        }
        self.intel.hover_hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_HIDE_DELAY).await;
            let _ = this.update(cx, |view, cx| {
                if !view.intel.card_hovered {
                    view.intel.hover_card = None;
                    view.intel.hover_word = None;
                    view.intel.hover_request = 0;
                    cx.notify();
                }
            });
        }));
    }

    fn set_card_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.intel.card_hovered = hovered;
        if hovered {
            self.intel.hover_hide_task = None;
        } else {
            self.schedule_hover_hide(cx);
        }
    }

    /// Closes the hover card immediately and forgets any pending request.
    pub(super) fn dismiss_hover(&mut self, cx: &mut Context<Self>) {
        let shown = self.intel.hover_card.is_some();
        self.intel.hover_pending = None;
        self.intel.hover_word = None;
        self.intel.hover_card = None;
        self.intel.hover_task = None;
        self.intel.hover_hide_task = None;
        self.intel.hover_request = 0;
        self.intel.card_hovered = false;
        if shown {
            cx.notify();
        }
    }

    // ---- go to definition -------------------------------------------------

    /// Ctrl/Cmd+click: asks for the definition under the pointer. Returns
    /// whether the click was consumed.
    pub(super) fn definition_click(&mut self, x: f32, y: f32, cx: &mut Context<Self>) -> bool {
        if !self.intel.enabled || y >= self.layout.viewport_height {
            return false;
        }
        let (rem_x, rem_y) = self.remainders();
        let Some(byte) = self.surface.byte_under_pointer(x + rem_x, y + rem_y) else {
            return false;
        };
        let text = self.surface.document().text();
        if intel::word_range(text, byte).is_none() {
            return false;
        }
        self.dismiss_hover(cx);
        self.close_completion(cx);
        self.emit_definition(byte, cx);
        true
    }

    // ---- completion -------------------------------------------------------

    /// Called after an edit routed through the input pipeline.
    pub(super) fn after_edit(&mut self, kind: EditKind, cx: &mut Context<Self>) {
        if !self.intel.enabled || self.read_only {
            return;
        }
        self.dismiss_hover(cx);
        let Some(caret) = self.primary_caret() else {
            self.close_completion(cx);
            return;
        };
        match kind {
            EditKind::Typed(c) if intel::is_word_char(c) => {
                if !self.refilter_completion(caret, cx) {
                    self.schedule_completion(None, cx);
                }
            }
            EditKind::Typed(c) if self.is_trigger(c, caret) => {
                self.close_completion(cx);
                self.schedule_completion(Some(c), cx);
            }
            EditKind::Deleted => {
                if self.intel.completion.is_some() {
                    self.refilter_completion(caret, cx);
                }
            }
            _ => self.close_completion(cx),
        }
    }

    fn is_trigger(&self, c: char, caret: usize) -> bool {
        match c {
            '.' => true,
            ':' => caret >= 2 && self.surface.document().text().char_at(caret - 2) == Some(':'),
            _ => false,
        }
    }

    /// Re-ranks the open menu for the text typed since its anchor. Returns
    /// whether a menu is still open.
    fn refilter_completion(&mut self, caret: usize, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.intel.completion.clone() else {
            return false;
        };
        let anchor = session.borrow().anchor;
        if caret < anchor {
            self.close_completion(cx);
            return false;
        }
        let prefix = self
            .surface
            .document()
            .text()
            .slice(anchor..caret)
            .into_owned();
        if !prefix.chars().all(intel::is_word_char) {
            self.close_completion(cx);
            return false;
        }
        session.borrow_mut().refilter(&prefix);
        if session.borrow().is_empty() {
            self.close_completion(cx);
            return false;
        }
        popups::reveal(&self.intel.completion_scroll, session.borrow().selected());
        self.refresh_docs(cx);
        cx.notify();
        true
    }

    fn schedule_completion(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        self.intel.completion_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMPLETION_DELAY).await;
            let _ = this.update(cx, |view, cx| view.request_completion(trigger, cx));
        }));
    }

    fn request_completion(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        self.intel.completion_task = None;
        let Some(caret) = self.primary_caret() else {
            return;
        };
        let anchor = if trigger.is_some() {
            caret
        } else {
            intel::word_start_before(self.surface.document().text(), caret)
        };
        let Some(request) = self.lsp_request(caret) else {
            return;
        };
        self.intel.completion_request = request.id;
        self.intel.completion_anchor = anchor;
        self.intel.completion_caret = caret;
        cx.emit(EditorEvent::CompletionRequested { request, trigger });
    }

    pub(super) fn close_completion(&mut self, cx: &mut Context<Self>) {
        let open = self.intel.completion.take().is_some();
        self.intel.completion_docs = None;
        self.intel.completion_task = None;
        self.intel.resolve_task = None;
        self.intel.completion_request = 0;
        self.intel.resolved.clear();
        if open {
            cx.notify();
        }
    }

    /// Rebuilds the documentation panel for the selected entry, and asks the
    /// host to resolve it when nothing is known yet.
    fn refresh_docs(&mut self, cx: &mut Context<Self>) {
        self.intel.resolve_task = None;
        self.intel.completion_docs = None;
        let Some(session) = self.intel.completion.clone() else {
            return;
        };
        let (host_data, request, markdown_text) = {
            let session = session.borrow();
            let Some(entry) = session.selected_entry() else {
                return;
            };
            (
                entry.host_data,
                session.request,
                popups::entry_doc_blocks(entry),
            )
        };
        if !markdown_text.is_empty() {
            let blocks = popups::build_blocks(
                &markdown::parse(&markdown_text),
                self.surface.language(),
                &self.surface.theme(),
            );
            self.intel.completion_docs = Some(DocsPanel {
                card: HoverCard::new(Vec::new(), blocks),
            });
        }
        if host_data != 0 && !self.intel.resolved.contains(&host_data) {
            self.intel.resolve_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RESOLVE_DELAY).await;
                let _ = this.update(cx, |_, cx| {
                    cx.emit(EditorEvent::CompletionResolveRequested { request, host_data });
                });
            }));
        }
    }

    /// Menu navigation. Returns whether the key was consumed.
    pub(super) fn completion_key(
        &mut self,
        key: &str,
        shift: bool,
        control: bool,
        alt: bool,
        platform: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = self.intel.completion.clone() else {
            return false;
        };
        let plain = !control && !alt && !platform;
        match key {
            "down" if plain => session.borrow_mut().move_selection(1),
            "up" if plain => session.borrow_mut().move_selection(-1),
            "pagedown" if plain => session
                .borrow_mut()
                .move_selection_clamped(popups::MENU_MAX_ROWS as isize),
            "pageup" if plain => session
                .borrow_mut()
                .move_selection_clamped(-(popups::MENU_MAX_ROWS as isize)),
            "enter" | "tab" if plain && !shift => {
                self.accept_completion(None, cx);
                return true;
            }
            "escape" => {
                self.close_completion(cx);
                return true;
            }
            _ => return false,
        }
        popups::reveal(&self.intel.completion_scroll, session.borrow().selected());
        self.refresh_docs(cx);
        cx.notify();
        true
    }

    /// Inserts the entry at `rank` (or the selected one).
    pub(super) fn accept_completion(&mut self, rank: Option<usize>, cx: &mut Context<Self>) {
        let Some(session) = self.intel.completion.clone() else {
            return;
        };
        let (entry, anchor) = {
            let session = session.borrow();
            let entry = match rank {
                Some(rank) => session.entry(rank),
                None => session.selected_entry(),
            };
            match entry {
                Some(entry) => (entry.clone(), session.anchor),
                None => return,
            }
        };
        let Some(caret) = self.primary_caret() else {
            self.close_completion(cx);
            return;
        };
        if caret < anchor || self.read_only {
            self.close_completion(cx);
            return;
        }
        let (text, selection) = if entry.is_snippet {
            let expanded = intel::expand_snippet(&entry.insert_text);
            (expanded.text, expanded.selection)
        } else {
            let end = entry.insert_text.len();
            (entry.insert_text.clone(), end..end)
        };

        let mut transaction = Transaction::new().replace(anchor..caret, text);
        // Edits before the anchor (an added `use`) shift where the text lands.
        let mut shift: isize = 0;
        for edit in &entry.additional_edits {
            let before_anchor =
                edit.range.end < anchor || (edit.range.end == anchor && edit.range.start < anchor);
            if before_anchor {
                transaction = transaction.replace(edit.range.clone(), edit.new_text.clone());
                shift += edit.new_text.len() as isize - edit.range.len() as isize;
            }
        }

        self.close_completion(cx);
        self.sync_surface_to_target();
        if self
            .surface
            .apply_transaction(&transaction, Grouping::Separate)
            .is_err()
        {
            return;
        }
        let base = (anchor as isize + shift).max(0) as usize;
        self.surface
            .editor_mut()
            .set_selections(SelectionSet::new([Selection::range(
                base + selection.start,
                base + selection.end,
            )]));
        self.surface.reveal_primary_caret();
        self.retarget_from_surface();
        cx.emit(EditorEvent::Changed);
        if entry.host_data != 0 {
            cx.emit(EditorEvent::CompletionCommitted {
                host_data: entry.host_data,
            });
        }
        cx.notify();
    }

    // ---- overlay ----------------------------------------------------------

    /// Window-space left edge, line top and line bottom for `byte`, or
    /// `None` when it is scrolled out of view.
    fn overlay_position(&self, byte: usize) -> Option<(f32, f32, f32)> {
        let (text_x, text_y) = self.surface.byte_to_text_pixel(byte)?;
        let (rem_x, rem_y) = self.remainders();
        let top = self.intel.origin.y.to_f32() + text_y - rem_y;
        let limit = self.intel.origin.y.to_f32() + self.layout.viewport_height;
        let first = self.intel.origin.y.to_f32() - self.line_height;
        if top > limit - self.line_height || top < first {
            return None;
        }
        let left = self.intel.origin.x.to_f32() + self.layout.text_left + text_x - rem_x;
        Some((left, top, top + self.line_height))
    }

    /// The open popups, placed at the anchor word or caret.
    pub(super) fn render_overlays(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut overlays = Vec::new();
        if !self.intel.enabled {
            return overlays;
        }
        if self.intel.hover_card.is_none() {
            // A card that vanished under the pointer never reports leaving.
            self.intel.card_hovered = false;
        }
        let colors = PopupColors::from_theme(&self.surface.theme());
        let mono: SharedString = self.font_family.clone().into();
        let editor = cx.entity();
        let viewport_height = window.viewport_size().height.to_f32();

        if let Some(session) = self.intel.completion.clone() {
            let anchor = session.borrow().anchor;
            match self.overlay_position(anchor) {
                Some((left, top, bottom)) => {
                    let pick = editor.clone();
                    let on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)> =
                        Rc::new(move |rank, _window, cx| {
                            pick.update(cx, |view, cx| view.accept_completion(Some(rank), cx));
                        });
                    let height = popups::menu_height(session.borrow().len()).to_f32();
                    let menu = popups::completion_menu(
                        &session,
                        &self.intel.completion_scroll,
                        self.intel.completion_docs.as_ref(),
                        colors,
                        mono.clone(),
                        POPUP_TEXT_SIZE,
                        on_pick,
                    );
                    overlays.push(place(
                        menu,
                        left,
                        top,
                        bottom,
                        bottom + height < viewport_height,
                    ));
                }
                None => self.close_completion(cx),
            }
        }

        if self.intel.completion.is_none() {
            if let (Some(card), Some(word)) =
                (self.intel.hover_card.clone(), self.intel.hover_word.clone())
            {
                if let Some((left, top, bottom)) = self.overlay_position(word.start) {
                    let hover = editor.clone();
                    let element = popups::hover_card(
                        &card,
                        colors,
                        mono,
                        POPUP_TEXT_SIZE,
                        move |hovered: &bool, _window: &mut Window, cx: &mut App| {
                            let hovered = *hovered;
                            hover.update(cx, |view, cx| view.set_card_hovered(hovered, cx));
                        },
                    );
                    // The card's height is not known before layout; prefer
                    // below unless the word sits in the lower part of the window.
                    let below = bottom + popups::CARD_MAX_HEIGHT * 0.6 < viewport_height;
                    overlays.push(place(element, left, top, bottom, below));
                }
            }
        }
        overlays
    }
}

/// Places `element` below the line (or above it when there is no room); the
/// anchored wrapper keeps it inside the window.
fn place(element: AnyElement, left: f32, top: f32, bottom: f32, below: bool) -> AnyElement {
    let (corner, y) = if below {
        (Corner::TopLeft, bottom + 2.0)
    } else {
        (Corner::BottomLeft, top - 2.0)
    };
    deferred(
        anchored()
            .position(point(px(left), px(y)))
            .anchor(corner)
            .snap_to_window()
            .child(element),
    )
    .with_priority(10)
    .into_any_element()
}
