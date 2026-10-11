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

fn view_with<'a>(cx: &'a mut TestAppContext, text: &'static str) -> (wgpui::Entity<WgpuiEditorView>, &'a mut wgpui::VisualTestContext) {
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        let surface = EditorSurface::with_language(
            text,
            Default::default(),
            SurfaceGeometry::default(),
            Language::Rust,
        );
        let mut view = WgpuiEditorView::new(surface, cx);
        view.set_language_intelligence(true, cx);
        view
    });
    cx.update(|window, app| {
        let handle = wgpui::Focusable::focus_handle(view.read(app), app);
        window.focus(&handle, app);
    });
    cx.run_until_parked();
    (view, cx)
}

#[test]
fn typing_an_identifier_requests_completion() {
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "");
    let mut events = cx.events::<EditorEvent, _>(&view);
    cx.simulate_input("v");
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(
        seen.iter().any(|e| matches!(e, EditorEvent::CompletionRequested { .. })),
        "typing produced no completion request; events: {seen:?}"
    );
}

#[test]
fn a_host_answer_opens_the_completion_menu() {
    use mockaco_gpui::intel::CompletionEntry;
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "");
    let mut events = cx.events::<EditorEvent, _>(&view);
    cx.simulate_input("v");
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    let request = loop {
        match events.try_recv() {
            Ok(EditorEvent::CompletionRequested { request, .. }) => break request,
            Ok(_) => continue,
            Err(_) => panic!("no completion request"),
        }
    };
    view.update(cx, |view, cx| {
        view.show_completions(
            request.id,
            request.version,
            vec![CompletionEntry::new("value"), CompletionEntry::new("vec")],
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.completion_len()), 2);
}

fn requested_after_typing(typed: &[&str], start: &'static str) -> Option<Option<char>> {
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, start);
    let mut events = cx.events::<EditorEvent, _>(&view);
    for piece in typed {
        cx.simulate_input(piece);
    }
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    let mut last = None;
    while let Ok(event) = events.try_recv() {
        if let EditorEvent::CompletionRequested { trigger, .. } = event {
            last = Some(trigger);
        }
    }
    last
}

#[test]
fn trigger_characters_request_completion() {
    // (typed pieces, start text, expected trigger of the last request)
    let cases: &[(&[&str], &str, Option<char>)] = &[
        (&["."], "value", Some('.')),
        (&[":", ":"], "Vec", Some(':')),
        (&[","], "foo(a", Some(',')),
        (&[" "], "let x =", Some(' ')),
        (&["("], "foo", Some('(')),
        (&["["], "let a = x", Some('[')),
    ];
    for (typed, start, expected) in cases {
        assert_eq!(
            requested_after_typing(typed, start),
            Some(*expected),
            "typing {typed:?} after {start:?} should request completion triggered by {expected:?}"
        );
    }
}

#[test]
fn a_single_colon_is_not_a_trigger() {
    assert_eq!(requested_after_typing(&[":"], "let x"), None);
}

#[test]
fn server_registered_triggers_open_completion() {
    let mut cx = TestAppContext::single();
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let surface = EditorSurface::with_language(
            "fn f<T>(x: &T) -> &",
            Default::default(),
            SurfaceGeometry::default(),
            Language::Rust,
        );
        let mut view = WgpuiEditorView::new(surface, cx);
        view.set_language_intelligence(true, cx);
        view.set_completion_triggers(vec!['\'']);
        view
    });
    cx.update(|window, app| {
        let handle = wgpui::Focusable::focus_handle(view.read(app), app);
        window.focus(&handle, app);
    });
    let mut events = cx.events::<EditorEvent, _>(&view);
    cx.simulate_input("'");
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    let mut asked = None;
    while let Ok(event) = events.try_recv() {
        if let EditorEvent::CompletionRequested { trigger, .. } = event {
            asked = Some(trigger);
        }
    }
    assert_eq!(asked, Some(Some('\'')));
    // Without the registration the same character does nothing.
    assert_eq!(requested_after_typing(&["'"], "x"), None);
}
