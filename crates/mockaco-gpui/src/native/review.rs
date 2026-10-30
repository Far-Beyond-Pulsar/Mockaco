//! Inline review of a proposed rewrite.
//!
//! [`WgpuiEditorView::begin_review`] puts the proposal in the buffer, tints
//! the lines it added, and shows the lines it replaced in a block above each
//! changed region with Accept / Reject buttons. The editor is read-only while
//! the review is open. Hosts hear about progress through
//! [`EditorEvent::ReviewUpdated`] and [`EditorEvent::ReviewFinished`].

use super::{EditorEvent, WgpuiEditorView};
use crate::{SurfaceColor, SurfaceTheme};
use mockaco_core::{Edit, Grouping, Transaction};
use mockaco_diff::review::{Review, ReviewEdit};
use mockaco_renderer::{Block, BlockPlacement};
use std::collections::HashMap;
use wgpui::{
    div, hsla, px, AnyView, AppContext, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, SharedString, Styled, WeakEntity, Window,
};

/// Review blocks use ids from here up, leaving the rest to the host.
pub const REVIEW_BLOCK_BASE: u64 = 1 << 48;

const ADDED_TINT: SurfaceColor = SurfaceColor::rgba(46, 160, 67, 48);
const REMOVED_TINT: SurfaceColor = SurfaceColor::rgba(248, 81, 73, 44);

pub(super) struct ReviewState {
    review: Review,
    was_read_only: bool,
    accepted: usize,
    rejected: usize,
    /// Which hunk the caret keys (Alt+Up / Alt+Down) are on.
    focus: Option<u64>,
}

impl WgpuiEditorView {
    /// Applies `proposed` to the buffer and opens a review of what changed.
    /// Returns the number of changed regions; `0` leaves everything as it was
    /// (identical text, or a review already open).
    pub fn begin_review(&mut self, proposed: &str, cx: &mut Context<Self>) -> usize {
        if self.review.is_some() {
            return 0;
        }
        let base = self.text();
        let review = Review::new(&base, proposed);
        if review.is_done() {
            return 0;
        }
        let base_snapshot = mockaco_core::Document::new(base.as_str()).snapshot();
        let proposed_snapshot = mockaco_core::Document::new(proposed).snapshot();
        let edits = review.forward_edits(&base_snapshot, &proposed_snapshot);
        let was_read_only = self.read_only;
        self.read_only = false;
        if !self.apply_review_edits(&edits, cx) {
            self.read_only = was_read_only;
            return 0;
        }
        self.read_only = true;
        let count = review.pending().len();
        let first = review.pending().first().map(|hunk| hunk.id);
        self.review = Some(ReviewState {
            review,
            was_read_only,
            accepted: 0,
            rejected: 0,
            focus: first,
        });
        self.refresh_review(cx);
        self.reveal_review_focus(cx);
        cx.emit(EditorEvent::ReviewUpdated { pending: count });
        count
    }

    /// Whether a review is open.
    pub fn is_reviewing(&self) -> bool {
        self.review.is_some()
    }

    /// Hunks still awaiting a decision.
    pub fn review_pending(&self) -> usize {
        self.review
            .as_ref()
            .map_or(0, |state| state.review.pending().len())
    }

    pub fn accept_review_hunk(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(state) = self.review.as_mut() else {
            return;
        };
        if state.review.accept(id) {
            state.accepted += 1;
        }
        self.after_review_decision(id, cx);
    }

    pub fn reject_review_hunk(&mut self, id: u64, cx: &mut Context<Self>) {
        let snapshot = self.surface.document().clone();
        let Some(state) = self.review.as_mut() else {
            return;
        };
        let Some(edit) = state.review.reject(id, &snapshot) else {
            return;
        };
        state.rejected += 1;
        self.read_only = false;
        let applied = self.apply_review_edits(&[edit], cx);
        self.read_only = true;
        if applied {
            cx.emit(EditorEvent::Changed);
        }
        self.after_review_decision(id, cx);
    }

    /// Keeps every remaining hunk and closes the review.
    pub fn accept_all_review(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.review.as_mut() else {
            return;
        };
        state.accepted += state.review.pending().len();
        state.review.accept_all();
        self.finish_review(cx);
    }

    /// Restores the original lines for every remaining hunk and closes the
    /// review.
    pub fn reject_all_review(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.surface.document().clone();
        let Some(state) = self.review.as_mut() else {
            return;
        };
        state.rejected += state.review.pending().len();
        let edits = state.review.reject_all(&snapshot);
        self.read_only = false;
        let applied = self.apply_review_edits(&edits, cx);
        self.read_only = true;
        if applied {
            cx.emit(EditorEvent::Changed);
        }
        self.finish_review(cx);
    }

    /// Moves the review focus to the next (or previous) pending hunk and
    /// scrolls to it.
    pub fn goto_review_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(state) = self.review.as_mut() else {
            return;
        };
        let pending = state.review.pending();
        if pending.is_empty() {
            return;
        }
        let current = state
            .focus
            .and_then(|id| pending.iter().position(|hunk| hunk.id == id));
        let next = match (current, forward) {
            (Some(index), true) => (index + 1) % pending.len(),
            (Some(index), false) => (index + pending.len() - 1) % pending.len(),
            (None, _) => 0,
        };
        state.focus = Some(pending[next].id);
        self.refresh_review(cx);
        self.reveal_review_focus(cx);
    }

    /// Handles the review's keys; returns whether `key` was one of them.
    pub(super) fn review_key(
        &mut self,
        key: &str,
        control: bool,
        alt: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.review.is_none() {
            return false;
        }
        match (key, control, alt) {
            ("enter", true, false) => self.accept_all_review(cx),
            ("escape", false, false) => self.reject_all_review(cx),
            ("down", false, true) => self.goto_review_hunk(true, cx),
            ("up", false, true) => self.goto_review_hunk(false, cx),
            _ => return false,
        }
        true
    }

    fn after_review_decision(&mut self, decided: u64, cx: &mut Context<Self>) {
        let Some(state) = self.review.as_mut() else {
            return;
        };
        if state.review.is_done() {
            self.finish_review(cx);
            return;
        }
        if state.focus == Some(decided) {
            state.focus = state.review.pending().first().map(|hunk| hunk.id);
        }
        let pending = state.review.pending().len();
        self.refresh_review(cx);
        cx.emit(EditorEvent::ReviewUpdated { pending });
    }

    fn finish_review(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.review.take() else {
            return;
        };
        self.read_only = state.was_read_only;
        self.review_backgrounds.clear();
        self.remove_review_blocks();
        cx.emit(EditorEvent::ReviewFinished {
            accepted: state.accepted,
            rejected: state.rejected,
        });
        cx.notify();
    }

    fn apply_review_edits(&mut self, edits: &[ReviewEdit], cx: &mut Context<Self>) -> bool {
        let transaction = Transaction::from_edits(
            edits
                .iter()
                .map(|edit| Edit::replace(edit.range.clone(), edit.text.clone())),
        );
        let applied = self
            .surface
            .apply_transaction(&transaction, Grouping::Separate)
            .is_ok();
        if applied {
            cx.emit(EditorEvent::Changed);
        }
        applied
    }

    fn remove_review_blocks(&mut self) {
        self.block_views.retain(|id, _| *id < REVIEW_BLOCK_BASE);
        let kept: Vec<Block> = self
            .surface
            .blocks()
            .iter()
            .copied()
            .filter(|block| block.id < REVIEW_BLOCK_BASE)
            .collect();
        self.surface.set_blocks(kept);
    }

    /// Rebuilds the tints and the removed-lines blocks from the pending
    /// hunks.
    fn refresh_review(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.review.as_ref() else {
            return;
        };
        let focus = state.focus;
        let line_count = self.surface.document().position_map().line_count();
        let last_line = line_count.saturating_sub(1);
        let editor = cx.entity().downgrade();
        let theme = self.surface.theme();
        let font_family: SharedString = self.font_family.clone().into();

        let mut backgrounds = HashMap::new();
        let mut blocks: Vec<Block> = self
            .surface
            .blocks()
            .iter()
            .copied()
            .filter(|block| block.id < REVIEW_BLOCK_BASE)
            .collect();
        self.block_views.retain(|id, _| *id < REVIEW_BLOCK_BASE);
        for hunk in state.review.pending() {
            for line in hunk.added.clone() {
                backgrounds.insert(line, ADDED_TINT);
            }
            let id = REVIEW_BLOCK_BASE + hunk.id;
            let (line, placement) = if hunk.added.start <= last_line {
                (hunk.added.start, BlockPlacement::Above)
            } else {
                (last_line, BlockPlacement::Below)
            };
            blocks.push(Block {
                id,
                line,
                placement,
                rows: 1 + hunk.removed.len() as u32,
            });
            let view = cx.new(|_| HunkView {
                id: hunk.id,
                removed: hunk.removed.clone(),
                added: hunk.added.len(),
                focused: focus == Some(hunk.id),
                editor: editor.clone(),
                theme: theme.clone(),
                font_family: font_family.clone(),
                font_size: self.font_size,
                line_height: self.line_height,
            });
            self.block_views.insert(id, AnyView::from(view));
        }
        self.review_backgrounds = backgrounds;
        self.surface.set_blocks(blocks);
        cx.notify();
    }

    fn reveal_review_focus(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.review.as_ref().and_then(|state| state.focus) else {
            return;
        };
        if let Some(line) = self.block_line(REVIEW_BLOCK_BASE + id) {
            self.go_to(line, 0, cx);
        }
    }
}

/// One hunk's block: a header with the actions, then the replaced lines.
struct HunkView {
    id: u64,
    removed: Vec<String>,
    added: usize,
    focused: bool,
    editor: WeakEntity<WgpuiEditorView>,
    theme: SurfaceTheme,
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
}

fn color(color: SurfaceColor) -> Hsla {
    let rgba = wgpui::Rgba {
        r: color.red as f32 / 255.0,
        g: color.green as f32 / 255.0,
        b: color.blue as f32 / 255.0,
        a: color.alpha as f32 / 255.0,
    };
    rgba.into()
}

impl Render for HunkView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let foreground = color(self.theme.foreground);
        let muted = color(self.theme.gutter_foreground);
        let line_height = px(self.line_height);
        let accept = hsla(0.36, 0.55, 0.62, 1.0);
        let reject = hsla(0.0, 0.70, 0.68, 1.0);
        let summary = match (self.removed.len(), self.added) {
            (0, added) => format!("+{added}"),
            (removed, 0) => format!("−{removed}"),
            (removed, added) => format!("−{removed} +{added}"),
        };
        let button = |label: &'static str, tint: Hsla, act: fn(&mut WgpuiEditorView, u64, &mut Context<WgpuiEditorView>)| {
            let editor = self.editor.clone();
            let id = self.id;
            div()
                .id(label)
                .px(px(8.0))
                .h(line_height)
                .flex()
                .items_center()
                .cursor_pointer()
                .text_color(tint)
                .hover(move |style| style.bg(hsla(tint.h, tint.s, tint.l, 0.16)))
                .child(label)
                .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                    cx.stop_propagation();
                    if let Some(editor) = editor.upgrade() {
                        editor.update(cx, |view, cx| act(view, id, cx));
                    }
                })
        };
        let header = div()
            .h(line_height)
            .flex()
            .items_center()
            .justify_between()
            .px(px(8.0))
            .bg(color(SurfaceColor::rgba(
                self.theme.foreground.red,
                self.theme.foreground.green,
                self.theme.foreground.blue,
                if self.focused { 30 } else { 16 },
            )))
            .text_size(px((self.font_size - 1.0).max(10.0)))
            .text_color(muted)
            .child(summary)
            .child(
                div()
                    .flex()
                    .child(button("Reject", reject, WgpuiEditorView::reject_review_hunk))
                    .child(button("Accept", accept, WgpuiEditorView::accept_review_hunk)),
            );
        let removed = self.removed.iter().map(|line| {
            div()
                .h(line_height)
                .px(px(8.0))
                .flex()
                .items_center()
                .whitespace_nowrap()
                .overflow_hidden()
                .bg(color(REMOVED_TINT))
                .font_family(self.font_family.clone())
                .text_size(px(self.font_size))
                .text_color(foreground)
                .child(SharedString::from(line.replace('\t', "    ")))
        });
        div()
            .size_full()
            .overflow_hidden()
            .bg(color(self.theme.background))
            .child(header)
            .children(removed)
    }
}

