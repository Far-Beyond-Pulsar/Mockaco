//! Per-frame cost of the editor's scroll path against huge documents.
//!
//! `cargo run --release -p mockaco-gpui --example scroll_profile [lines]`
//!
//! Prints the worst and mean time of each step a frame takes while scrolling
//! (scroll the surface, derive the frame, lay out row text, read the minimap)
//! so a regression that makes document length matter shows up as a number.

use mockaco_core::{Grouping, Transaction};
use mockaco_gpui::chrome;
use mockaco_gpui::{EditorSurface, Language, SurfaceGeometry};
use mockaco_renderer::DisplayConfig;
use std::time::{Duration, Instant};

fn source(lines: usize) -> String {
    let mut text = String::with_capacity(lines * 40);
    let mut n = 0usize;
    while n * 12 < lines {
        text.push_str(&format!(
            "/// Item {n}.\npub fn item_{n}(value: u64) -> u64 {{\n    // add {n}\n    let x = value + {n};\n    if x > 10 {{\n        x * 2\n    }} else {{\n        \"text\".len() as u64\n    }}\n}}\n\n\n"
        ));
        n += 1;
    }
    text
}

struct Stat {
    name: &'static str,
    samples: Vec<Duration>,
}

impl Stat {
    fn new(name: &'static str) -> Self {
        Self { name, samples: Vec::new() }
    }
    fn time<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let start = Instant::now();
        let result = f();
        self.samples.push(start.elapsed());
        result
    }
    fn report(&self) {
        let max = self.samples.iter().max().copied().unwrap_or_default();
        let total: Duration = self.samples.iter().sum();
        let mean = total / self.samples.len().max(1) as u32;
        println!(
            "{:<28} mean {:>9.3} ms   max {:>9.3} ms   ({} samples)",
            self.name,
            mean.as_secs_f64() * 1e3,
            max.as_secs_f64() * 1e3,
            self.samples.len()
        );
    }
}

fn main() {
    let lines: usize = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000_000);
    let start = Instant::now();
    let text = source(lines);
    println!(
        "generated {} lines, {:.1} MB in {:?}",
        text.bytes().filter(|b| *b == b'\n').count(),
        text.len() as f64 / 1e6,
        start.elapsed()
    );

    provider_breakdown(&text);
    for language in [Language::Rust, Language::from_extension("ts")] {
        println!("\n== language: {}", language.id());
        let start = Instant::now();
        let mut surface = EditorSurface::with_language(
            text.clone(),
            DisplayConfig::unwrapped(),
            SurfaceGeometry {
                width: 1200.0,
                height: 1000.0,
                line_height: 20.0,
                character_width: 8.4,
                gutter_character_width: 8.4,
            },
            language,
        );
        println!("open                         {:?}", start.elapsed());

        let rows = surface.display().row_count();
        let mut scroll = Stat::new("scroll_to (smooth step)");
        let mut jump = Stat::new("scroll_to (jump)");
        let mut poll = Stat::new("poll_language");
        let mut frame = Stat::new("render_frame_with_buffer");
        let mut row_text = Stat::new("layout_row_text (visible)");
        let mut minimap = Stat::new("minimap visible lines");
        let mut colored = Stat::new("jump -> highlighted (async)");
        let comment = language.line_comment();

        let paint = |surface: &mut EditorSurface, row: usize,
                         frame: &mut Stat, row_text: &mut Stat, minimap: &mut Stat, poll: &mut Stat| {
            poll.time(|| surface.poll_language());
            let painted = frame.time(|| surface.render_frame_with_buffer(1));
            row_text.time(|| {
                painted
                    .rows
                    .iter()
                    .map(|row| chrome::layout_row_text(row, 4, 0, 160).text.len())
                    .sum::<usize>()
            });
            minimap.time(|| {
                let layout = chrome::minimap_layout(rows, row as f32 * 20.0, rows as f32 * 20.0, 1000.0, 20.0, 1000.0);
                let snapshot = surface.document();
                let map = snapshot.position_map();
                (layout.first_line..layout.last_line)
                    .map(|line| {
                        let text = snapshot.text().line(line).unwrap();
                        chrome::minimap_line(&text, comment).len()
                    })
                    .sum::<usize>()
            });
        };

        // A smooth scroll: a few rows per frame across a stretch of the doc.
        let mut row = 0usize;
        for _ in 0..600 {
            row = (row + 3).min(rows - 1);
            scroll.time(|| surface.scroll_to(row, 0));
            paint(&mut surface, row, &mut frame, &mut row_text, &mut minimap, &mut poll);
        }
        // Scrollbar drags: arbitrary jumps through the whole document.
        for step in 0..100 {
            let target = (step * 7919 * 13) % rows;
            jump.time(|| surface.scroll_to(target, 0));
            paint(&mut surface, target, &mut frame, &mut row_text, &mut minimap, &mut poll);
            colored.time(|| surface.flush_language());
        }
        for stat in [&scroll, &jump, &poll, &frame, &row_text, &minimap, &colored] {
            stat.report();
        }

        let mut edit = Stat::new("keystroke (insert 1 char)");
        surface.scroll_to(rows / 2, 0);
        let at = surface.document().position_map().line_start(rows / 2).unwrap();
        for i in 0..20 {
            edit.time(|| {
                surface
                    .apply_transaction(&Transaction::new().insert(at + i, "x"), Grouping::Separate)
                    .unwrap()
            });
        }
        edit.report();
    }
}

#[allow(dead_code)]
fn provider_breakdown(text: &str) {
    use mockaco_language::{FoldingProvider, RustTreeSitterProvider, SyntaxHighlightProvider};
    let document = mockaco_core::Document::new(text.to_owned());
    let snapshot = document.snapshot();
    let map = snapshot.position_map();
    let provider = RustTreeSitterProvider::new().unwrap();
    let mut highlight = Stat::new("rust highlight (cold window)");
    let mut folds = Stat::new("rust fold_ranges (same window)");
    for step in 0..30 {
        let line = (step * 7919 * 13) % map.line_count();
        let start = map.line_start(line).unwrap();
        let end = map.line_end((line + 150).min(map.line_count() - 1)).unwrap();
        highlight.time(|| provider.highlight(&snapshot, start..end).unwrap().tokens.len());
        folds.time(|| provider.fold_ranges(&snapshot, start..end).unwrap().len());
    }
    highlight.report();
    folds.report();
}
