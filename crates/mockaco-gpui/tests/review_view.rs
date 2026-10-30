//! Inline review: a proposal is applied up front, each hunk gets a block with
//! its replaced lines, and Accept / Reject resolve it hunk by hunk.
#![cfg(feature = "native-wgpui")]

use mockaco_gpui::native::{EditorEvent, WgpuiEditorView};
use mockaco_gpui::{EditorSurface, Language, SurfaceGeometry};
use std::cell::RefCell;
use std::rc::Rc;
use wgpui::TestAppContext;

const BASE: &str = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
const PROPOSED: &str = "// header\nfn a() {}\nfn B() {}\nfn c() {}\n";

fn view_with<'a>(
    cx: &'a mut TestAppContext,
    text: &'static str,
) -> (
    wgpui::Entity<WgpuiEditorView>,
    &'a mut wgpui::VisualTestContext,
) {
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        let surface = EditorSurface::with_language(
            text,
            Default::default(),
            SurfaceGeometry::default(),
            Language::Rust,
        );
        WgpuiEditorView::new(surface, cx)
    });
    cx.run_until_parked();
    (view, cx)
}

fn text(view: &wgpui::Entity<WgpuiEditorView>, cx: &mut wgpui::VisualTestContext) -> String {
    view.read_with(cx, |view, _| view.text())
}

fn record_events(
    view: &wgpui::Entity<WgpuiEditorView>,
    cx: &mut wgpui::VisualTestContext,
) -> Rc<RefCell<Vec<EditorEvent>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &EditorEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    events
}

#[test]
fn begin_review_applies_the_proposal_and_locks_the_editor() {
    let mut cx = TestAppContext::single();
    let cx = &mut cx;
    let (view, cx) = view_with(cx, BASE);
    let count = view.update(cx, |view, cx| view.begin_review(PROPOSED, cx));
    assert!(count >= 2);
    assert_eq!(text(&view, cx), PROPOSED);
    view.read_with(cx, |view, _| {
        assert!(view.is_reviewing() && view.read_only);
        assert_eq!(view.review_pending(), count);
        // One block per hunk reserves rows for the header and removed lines.
        assert_eq!(view.surface.blocks().len(), count);
    });
}

#[test]
fn rejecting_every_hunk_restores_the_original() {
    let mut cx = TestAppContext::single();
    let cx = &mut cx;
    let (view, cx) = view_with(cx, BASE);
    let events = record_events(&view, cx);
    view.update(cx, |view, cx| view.begin_review(PROPOSED, cx));
    view.update(cx, |view, cx| view.reject_all_review(cx));
    assert_eq!(text(&view, cx), BASE);
    view.read_with(cx, |view, _| {
        assert!(!view.is_reviewing() && !view.read_only);
        assert!(view.surface.blocks().is_empty());
    });
    assert!(matches!(
        events.borrow().last(),
        Some(EditorEvent::ReviewFinished { accepted: 0, rejected }) if *rejected >= 2
    ));
}

#[test]
fn hunks_resolve_one_at_a_time_and_the_last_decision_closes_the_review() {
    let mut cx = TestAppContext::single();
    let cx = &mut cx;
    let (view, cx) = view_with(cx, BASE);
    let events = record_events(&view, cx);
    let count = view.update(cx, |view, cx| view.begin_review(PROPOSED, cx));
    assert!(count >= 2);
    // Keep the first hunk (the new header), throw away the rest.
    let ids: Vec<u64> = view.read_with(cx, |view, _| {
        view.surface.blocks().iter().map(|block| block.id).collect()
    });
    let first = ids[0] - (1 << 48);
    view.update(cx, |view, cx| view.accept_review_hunk(first, cx));
    assert_eq!(view.read_with(cx, |view, _| view.review_pending()), count - 1);
    assert_eq!(text(&view, cx), PROPOSED);
    view.update(cx, |view, cx| view.reject_all_review(cx));
    let result = text(&view, cx);
    assert!(result.starts_with("// header\n"), "{result:?}");
    assert!(result.contains("fn b() {}") && result.contains("fn d() {}"));
    assert!(matches!(
        events.borrow().last(),
        Some(EditorEvent::ReviewFinished { accepted: 1, .. })
    ));
}

#[test]
fn accepting_everything_keeps_the_proposal_and_unlocks() {
    let mut cx = TestAppContext::single();
    let cx = &mut cx;
    let (view, cx) = view_with(cx, BASE);
    view.update(cx, |view, cx| view.begin_review(PROPOSED, cx));
    view.update(cx, |view, cx| view.accept_all_review(cx));
    assert_eq!(text(&view, cx), PROPOSED);
    view.read_with(cx, |view, _| {
        assert!(!view.is_reviewing() && !view.read_only);
    });
}

#[test]
fn identical_text_opens_no_review() {
    let mut cx = TestAppContext::single();
    let cx = &mut cx;
    let (view, cx) = view_with(cx, BASE);
    assert_eq!(view.update(cx, |view, cx| view.begin_review(BASE, cx)), 0);
    assert!(!view.read_with(cx, |view, _| view.is_reviewing()));
}
