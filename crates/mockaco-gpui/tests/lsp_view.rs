//! Drives the real editor view through gpui's test harness: a simulated
//! pointer must produce hover requests, and answers must produce popups.
#![cfg(feature = "native-wgpui")]

use mockaco_gpui::native::{EditorEvent, WgpuiEditorView};
use mockaco_gpui::{EditorSurface, Language, SurfaceGeometry};
use std::time::Duration;
use wgpui::{point, px, Modifiers, TestAppContext};

#[test]
fn pointer_rest_requests_hover() {
    let mut cx = TestAppContext::single();
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let surface = EditorSurface::with_language(
            "fn main() {\n    let value = 1;\n}\n",
            Default::default(),
            SurfaceGeometry::default(),
            Language::Rust,
        );
        let mut view = WgpuiEditorView::new(surface, cx);
        view.set_language_intelligence(true, cx);
        view
    });
    let mut events = cx.events::<EditorEvent, _>(&view);
    cx.run_until_parked();
    // Rest on "value" (line 2).
    cx.simulate_mouse_move(point(px(120.0), px(30.0)), None, Modifiers::default());
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(
        seen.iter().any(|e| matches!(e, EditorEvent::HoverRequested(_))),
        "no hover request; events: {seen:?}"
    );
}
