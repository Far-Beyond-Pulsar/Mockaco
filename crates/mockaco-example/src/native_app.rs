use crate::{DemoAction, DemoState, DEMO_SOURCE};
use mockaco_core::{Document, Selection, SelectionSet};
use mockaco_diff::DiffEditor;
use mockaco_gpui::{native::WgpuiEditorView, EditorSurface, SurfaceGeometry};
use mockaco_renderer::{Decoration, DisplayConfig, WrapConfig};
use wgpui::{
    div, px, rgb, size, App, AppContext, Application, Bounds, Context, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Window, WindowBounds,
    WindowOptions,
};

const MODIFIED_SOURCE: &str =
    "fn main() {\n    let answer = 84;\n    println!(\"modified = {answer}\");\n}\n";

pub fn run() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1400.0), px(900.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(Showcase::new),
        )
        .expect("failed to open Mockaco showcase window");
        cx.activate(true);
    });
}

pub struct Showcase {
    state: DemoState,
    editor: Entity<WgpuiEditorView>,
    diff_original: Entity<WgpuiEditorView>,
    diff_modified: Entity<WgpuiEditorView>,
    diff_contract: DiffEditor,
    show_diff: bool,
    show_decorations: bool,
}

impl Showcase {
    fn new(cx: &mut Context<Self>) -> Self {
        let geometry = SurfaceGeometry {
            width: 920.0,
            height: 560.0,
            line_height: 22.0,
            character_width: 8.0,
            gutter_character_width: 8.0,
        };
        let config = DisplayConfig::wrapped(96).with_wrap(Some(
            WrapConfig::new(96)
                .max_segments_per_line(8)
                .max_line_scan_bytes(512),
        ));
        let state = DemoState::new();
        let editor = cx.new(|_| {
            WgpuiEditorView::new(EditorSurface::new(DEMO_SOURCE, config.clone(), geometry))
        });
        let diff_original = cx.new(|_| {
            WgpuiEditorView::new_read_only(EditorSurface::new(
                DEMO_SOURCE,
                config.clone(),
                geometry,
            ))
        });
        let diff_modified =
            cx.new(|_| WgpuiEditorView::new(EditorSurface::new(MODIFIED_SOURCE, config, geometry)));
        let original = Document::new(DEMO_SOURCE).snapshot();
        let diff_contract = DiffEditor::new(&original, MODIFIED_SOURCE);
        Self {
            state,
            editor,
            diff_original,
            diff_modified,
            diff_contract,
            show_diff: false,
            show_decorations: false,
        }
    }

    fn dispatch(&mut self, action: DemoAction, cx: &mut Context<Self>) {
        self.state.apply(action);
        let text = self.state.text().to_owned();
        self.editor.update(cx, |view, cx| {
            view.surface.replace_text(text);
            cx.notify();
        });
        cx.notify();
    }

    fn editor_action(&mut self, action: EditorAction, cx: &mut Context<Self>) {
        self.editor.update(cx, |view, cx| {
            match action {
                EditorAction::MultiCursor => {
                    let text_len = view.surface.editor().document().len_bytes();
                    view.surface.editor_mut().set_selections(SelectionSet::new([
                        Selection::caret(0),
                        Selection::caret(text_len),
                    ]));
                }
                EditorAction::Ime => {
                    let _ = view
                        .surface
                        .editor_mut()
                        .update_composition(0..0, "// IME commit\n");
                    let _ = view.surface.commit_composition();
                }
                EditorAction::Undo => {
                    view.surface.undo();
                }
                EditorAction::Redo => {
                    view.surface.redo();
                }
                EditorAction::Fold => {
                    let line = view
                        .surface
                        .editor()
                        .selections()
                        .primary()
                        .and_then(|selection| {
                            view.surface
                                .editor()
                                .document()
                                .position_map()
                                .byte_to_line(selection.cursor())
                                .ok()
                        })
                        .unwrap_or(0);
                    view.surface.toggle_fold_at_line(line);
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    fn toggle_decorations(&mut self, cx: &mut Context<Self>) {
        self.show_decorations = !self.show_decorations;
        let show_decorations = self.show_decorations;
        self.editor.update(cx, |view, cx| {
            let decorations = if show_decorations {
                vec![Decoration::new(0..2, 7), Decoration::new(20..26, 3)]
            } else {
                Vec::new()
            };
            view.surface.set_decorations(decorations);
            cx.notify();
        });
        self.state.apply(DemoAction::LspResults);
        cx.notify();
    }

    fn button(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        action: DemoAction,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .bg(rgb(0x202d40))
            .text_color(rgb(0xc9d7e8))
            .rounded_sm()
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.dispatch(action, cx)))
    }

    fn editor_button(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        action: EditorAction,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .bg(rgb(0x2a3b54))
            .text_color(rgb(0xe0e9f5))
            .rounded_sm()
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.editor_action(action, cx)))
    }
}

#[derive(Debug, Clone, Copy)]
enum EditorAction {
    MultiCursor,
    Ime,
    Undo,
    Redo,
    Fold,
}

impl Render for Showcase {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let diff_button = div()
            .id("diff")
            .px_2()
            .py_1()
            .bg(rgb(0x294564))
            .text_color(rgb(0xe4edf9))
            .rounded_sm()
            .cursor_pointer()
            .child(if self.show_diff {
                "Hide diff"
            } else {
                "Show diff"
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_diff = !this.show_diff;
                cx.notify();
            }));
        let decoration_button = div()
            .id("decorations")
            .px_2()
            .py_1()
            .bg(rgb(0x294564))
            .text_color(rgb(0xe4edf9))
            .rounded_sm()
            .cursor_pointer()
            .child("Diagnostics")
            .on_click(cx.listener(|this, _, _, cx| this.toggle_decorations(cx)));
        let lifecycle_actions = div()
            .flex()
            .gap_1()
            .child(self.button(cx, "load", "Load", DemoAction::Load))
            .child(self.button(cx, "local-edit", "Local edit", DemoAction::LocalEdit))
            .child(self.button(cx, "replace", "Replace", DemoAction::ReplaceAll))
            .child(self.button(cx, "save-ok", "Save", DemoAction::SaveSuccess))
            .child(self.button(cx, "save-fail", "Fail", DemoAction::SaveFailure))
            .child(self.button(cx, "external", "External", DemoAction::ExternalConflict))
            .child(self.button(cx, "keep", "Keep local", DemoAction::KeepLocal))
            .child(self.button(cx, "reload", "Reload", DemoAction::ReloadExternal))
            .child(self.button(cx, "close-save", "Close/save", DemoAction::CloseSave))
            .child(self.button(
                cx,
                "close-discard",
                "Close/discard",
                DemoAction::CloseDiscard,
            ))
            .child(self.button(cx, "close-cancel", "Close/cancel", DemoAction::CloseCancel));
        let editor_actions = div()
            .flex()
            .gap_1()
            .child(self.editor_button(cx, "multi", "Multi-cursor", EditorAction::MultiCursor))
            .child(self.editor_button(cx, "ime", "IME", EditorAction::Ime))
            .child(self.editor_button(cx, "undo", "Undo", EditorAction::Undo))
            .child(self.editor_button(cx, "redo", "Redo", EditorAction::Redo))
            .child(self.editor_button(cx, "fold", "Fold", EditorAction::Fold));
        let showcase_actions = div()
            .flex()
            .gap_1()
            .child(self.button(cx, "lsp", "Mock LSP", DemoAction::LspResults))
            .child(diff_button)
            .child(decoration_button);

        let toolbar = div()
            .flex()
            .gap_3()
            .p_3()
            .bg(rgb(0x121d2c))
            .border_b_1()
            .border_color(rgb(0x263750))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0x7f96b3)).child("LIFECYCLE"))
                    .child(lifecycle_actions),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0x7f96b3)).child("EDITOR"))
                    .child(editor_actions),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0x7f96b3)).child("SHOWCASE"))
                    .child(showcase_actions),
            );

        let main_editor = div()
            .flex()
            .flex_1()
            .min_w(px(500.0))
            .flex_col()
            .border_1()
            .border_color(rgb(0x2a3b54))
            .bg(rgb(0x101925))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(0x25344a))
                    .text_color(rgb(0x9db2cd))
                    .child("main.rs  •  Rust  •  live editor"),
            )
            .child(self.editor.clone());
        let content = if self.show_diff {
            div()
                .flex()
                .flex_1()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .border_1()
                        .border_color(rgb(0x2a3b54))
                        .bg(rgb(0x111a27))
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(rgb(0x25344a))
                                .text_color(rgb(0x8fa4bd))
                                .child("ORIGINAL  •  read-only"),
                        )
                        .child(self.diff_original.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .border_1()
                        .border_color(rgb(0x2a3b54))
                        .bg(rgb(0x111a27))
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(rgb(0x25344a))
                                .text_color(rgb(0xa5c9b0))
                                .child("MODIFIED  •  editable"),
                        )
                        .child(self.diff_modified.clone()),
                )
        } else {
            main_editor
        };

        let diff_count = self.diff_contract.diff().hunks().len();
        div()
            .id("mockaco-showcase")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x0d1420))
            .text_color(rgb(0xd7e3f4))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .bg(rgb(0x121d2c))
                    .border_b_1()
                    .border_color(rgb(0x263750))
                    .child("Mockaco Editor Component Showcase")
                    .child("  •  Rust syntax  •  in-memory demo  •  native WGPUI"),
            )
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .p_3()
                    .child(content),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .bg(rgb(0x182333))
                    .border_t_1()
                    .border_color(rgb(0x263750))
                    .child(format!(
                        "{}   •   {}   •   diff hunks: {}",
                        self.state.status(),
                        self.state.lsp_status(),
                        diff_count
                    )),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0x8095af))
                    .child("Arrows/Home/End move by display row  •  Shift extends  •  gutter chevrons fold  •  double/triple click selects"),
            )
    }
}
