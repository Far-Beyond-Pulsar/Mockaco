//! The view's find/replace and caret commands, driven as a host's menus
//! drive them: matches are found, stepped through, replaced (one undo step
//! each), kept current after typing, and cleared; Add Cursor adds a caret.
#![cfg(feature = "native-wgpui")]

use mockaco_gpui::native::WgpuiEditorView;
use mockaco_gpui::{EditorSurface, Language, SurfaceGeometry};
use mockaco_renderer::{SEARCH_CURRENT_MATCH_STYLE_ID, SEARCH_MATCH_STYLE_ID};
use wgpui::TestAppContext;

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

/// The byte range of the primary selection.
fn selection(
    view: &wgpui::Entity<WgpuiEditorView>,
    cx: &mut wgpui::VisualTestContext,
) -> std::ops::Range<usize> {
    view.read_with(cx, |view, _| {
        view.surface
            .editor()
            .selections()
            .primary()
            .unwrap()
            .ordered_range()
    })
}

/// How many search highlights the surface paints (current and others).
fn highlights(
    view: &wgpui::Entity<WgpuiEditorView>,
    cx: &mut wgpui::VisualTestContext,
) -> (usize, usize) {
    view.read_with(cx, |view, _| {
        let frame = view.surface.render_frame();
        let count = |id| {
            frame
                .decoration_geometry
                .iter()
                .filter(|decoration| decoration.style.id == id)
                .count()
        };
        (
            count(SEARCH_CURRENT_MATCH_STYLE_ID),
            count(SEARCH_MATCH_STYLE_ID),
        )
    })
}

#[test]
fn find_selects_steps_and_wraps_through_matches() {
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "let foo = 1;\nlet Foo = foo + foo;\n");

    let count = view.update(cx, |view, cx| view.find("foo", true, cx));
    assert_eq!(count, 3, "case-sensitive: Foo is not a match");
    assert_eq!(selection(&view, cx), 4..7, "the first match is selected");
    assert_eq!(highlights(&view, cx), (1, 2));
    assert_eq!(
        view.read_with(cx, |view, _| view.find_status()),
        Some((Some(0), 3))
    );

    assert_eq!(view.update(cx, |view, cx| view.find_next(cx)), Some((1, 3)));
    assert_eq!(selection(&view, cx), 23..26);
    assert_eq!(view.update(cx, |view, cx| view.find_next(cx)), Some((2, 3)));
    assert_eq!(
        view.update(cx, |view, cx| view.find_next(cx)),
        Some((0, 3)),
        "wraps"
    );
    assert_eq!(
        view.update(cx, |view, cx| view.find_previous(cx)),
        Some((2, 3)),
        "and back"
    );

    let count = view.update(cx, |view, cx| view.find("foo", false, cx));
    assert_eq!(count, 4, "case-insensitive finds Foo too");

    view.update(cx, |view, cx| view.clear_find(cx));
    assert_eq!(highlights(&view, cx), (0, 0));
    assert_eq!(view.read_with(cx, |view, _| view.find_status()), None);
    assert_eq!(view.update(cx, |view, cx| view.find("", true, cx)), 0);
}

#[test]
fn replace_current_and_replace_all_are_single_undo_steps() {
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "a b a b a\n");

    view.update(cx, |view, cx| view.find("a", true, cx));
    assert!(view.update(cx, |view, cx| view.replace_current("x", cx)));
    assert_eq!(view.read_with(cx, |view, _| view.text()), "x b a b a\n");
    assert_eq!(selection(&view, cx), 4..5, "the next match is selected");
    assert_eq!(
        view.read_with(cx, |view, _| view.find_status()),
        Some((Some(0), 2))
    );

    view.update(cx, |view, cx| view.undo(cx));
    assert_eq!(view.read_with(cx, |view, _| view.text()), "a b a b a\n");
    assert_eq!(
        view.read_with(cx, |view, _| view.find_status())
            .map(|(_, count)| count),
        Some(3),
        "the search follows the undo"
    );

    assert_eq!(view.update(cx, |view, cx| view.replace_all("yy", cx)), 3);
    assert_eq!(view.read_with(cx, |view, _| view.text()), "yy b yy b yy\n");
    assert_eq!(
        view.read_with(cx, |view, _| view.find_status()),
        Some((None, 0))
    );
    view.update(cx, |view, cx| view.undo(cx));
    assert_eq!(
        view.read_with(cx, |view, _| view.text()),
        "a b a b a\n",
        "one step"
    );
}

#[test]
fn typing_keeps_the_highlights_on_the_current_text() {
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "one\none\n");
    view.update(cx, |view, cx| view.find("one", true, cx));
    view.update(cx, |view, cx| view.go_to(1, 3, cx));
    cx.update(|window, app| {
        let handle = wgpui::Focusable::focus_handle(view.read(app), app);
        window.focus(&handle, app);
    });
    cx.simulate_input(" one");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.text()), "one\none one\n");
    let total = view
        .read_with(cx, |view, _| view.find_status())
        .map(|(_, count)| count);
    assert_eq!(total, Some(3), "the typed match is found");
    let (current, others) = highlights(&view, cx);
    assert_eq!(current + others, 3);
}

#[test]
fn add_cursor_adds_a_caret_below_and_above() {
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "aaa\nbbb\nccc\n");
    view.update(cx, |view, cx| view.go_to(1, 1, cx));
    view.update(cx, |view, cx| view.add_cursor(false, cx));
    view.update(cx, |view, cx| view.add_cursor(true, cx));
    let carets = view.read_with(cx, |view, _| {
        view.surface
            .editor()
            .selections()
            .selections()
            .iter()
            .map(|selection| selection.cursor())
            .collect::<Vec<_>>()
    });
    assert_eq!(carets.len(), 3, "{carets:?}");
}

#[test]
fn find_query_takes_whole_word_and_regex_and_replaces_groups() {
    use mockaco_core::SearchQuery;
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "let a1 = b2;\nlet ab = a1;\n");

    let whole = view.update(cx, |view, cx| {
        view.find_query(SearchQuery::new("a1").whole_word(true), cx)
    });
    assert_eq!(whole, 2);
    let regex = view.update(cx, |view, cx| {
        view.find_query(SearchQuery::new(r"([a-z])(\d)").regex(true), cx)
    });
    assert_eq!(regex, 3, "a1, b2 and a1");
    let replaced = view.update(cx, |view, cx| view.replace_all("$2$1", cx));
    assert_eq!(replaced, 3);
    assert_eq!(
        view.read_with(cx, |view, _| view.text()),
        "let 1a = 2b;\nlet ab = 1a;\n"
    );

    // An invalid expression matches nothing and clears the highlights.
    let invalid = view.update(cx, |view, cx| {
        view.find_query(SearchQuery::new("(").regex(true), cx)
    });
    assert_eq!(invalid, 0);
    assert_eq!(highlights(&view, cx), (0, 0));
}

#[test]
fn add_next_occurrence_grows_the_selection_set() {
    let mut cx = TestAppContext::single();
    let (view, cx) = view_with(&mut cx, "foo + foo + bar + foo\n");
    let count = |view: &wgpui::Entity<WgpuiEditorView>, cx: &mut wgpui::VisualTestContext| {
        view.read_with(cx, |view, _| view.surface.editor().selections().len())
    };

    assert!(view.update(cx, |view, cx| view.add_next_occurrence(cx)));
    assert_eq!(selection(&view, cx), 0..3, "the word under the caret");
    assert_eq!(count(&view, cx), 1);
    assert!(view.update(cx, |view, cx| view.add_next_occurrence(cx)));
    assert!(view.update(cx, |view, cx| view.add_next_occurrence(cx)));
    assert_eq!(count(&view, cx), 3);
    assert_eq!(
        selection(&view, cx),
        18..21,
        "the last one added is primary"
    );
    assert!(
        !view.update(cx, |view, cx| view.add_next_occurrence(cx)),
        "no occurrence left"
    );

    // Typing replaces every occurrence at once.
    cx.update(|window, app| {
        let handle = wgpui::Focusable::focus_handle(view.read(app), app);
        window.focus(&handle, app);
    });
    cx.simulate_input("q");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, _| view.text()),
        "q + q + bar + q\n"
    );
}
