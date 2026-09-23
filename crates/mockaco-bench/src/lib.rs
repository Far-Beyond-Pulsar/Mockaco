//! Reproducible, framework-independent performance workloads for Mockaco.
//!
//! The binary emits machine-readable measurements. The Criterion target uses
//! these same workload functions, so quick smoke measurements and statistical
//! benchmark runs cannot drift apart.

use mockaco_core::{
    Document, DocumentMetadata, DocumentSession, SearchQuery, SearchSession, Transaction,
};
use mockaco_diff::compute_diff;
use mockaco_fixtures::{generated_document, pathological_document};
use mockaco_renderer::{
    DisplayConfig, DisplayMap, DisplayViewport, FoldRegion, FoldSet, WrapConfig,
};
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaseSpec {
    pub name: &'static str,
    pub target_bytes: usize,
    pub pathological: bool,
}

impl CaseSpec {
    pub const ALL: [Self; 4] = [
        Self {
            name: "1k",
            target_bytes: 1_024,
            pathological: false,
        },
        Self {
            name: "100k",
            target_bytes: 102_400,
            pathological: false,
        },
        Self {
            name: "1m",
            target_bytes: 1_048_576,
            pathological: false,
        },
        Self {
            name: "pathological-line",
            target_bytes: 1_048_577,
            pathological: true,
        },
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|case| case.name == name)
    }
}

impl fmt::Display for CaseSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

pub fn document_for(case: CaseSpec) -> String {
    if case.pathological {
        pathological_document(case.target_bytes)
    } else {
        generated_document(case.target_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentMetadataSummary {
    pub byte_count: usize,
    pub line_count: usize,
    pub max_line_bytes: usize,
    pub sha256: String,
}

pub fn metadata(text: &str) -> DocumentMetadataSummary {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    DocumentMetadataSummary {
        byte_count: text.len(),
        line_count: text.lines().count(),
        max_line_bytes: text.split('\n').map(str::len).max().unwrap_or(0),
        sha256: format!("{:x}", hasher.finalize()),
    }
}

pub fn open_snapshot(text: &str) -> u64 {
    let document = Document::new(text);
    document.snapshot().len_bytes() as u64
}

pub fn edit_transaction_mapping(text: &str) -> u64 {
    let mut document = Document::new(text);
    let offset = text.len() / 2;
    let applied = document
        .apply(&Transaction::new().insert(offset, "// edit\n"))
        .expect("generated insertion is valid UTF-8");
    applied
        .change_map
        .map_offset(offset, mockaco_core::Affinity::After) as u64
}

pub fn viewport_display_prepare(text: &str, viewport_lines: usize) -> u64 {
    let document = Document::new(text);
    let map = DisplayMap::new(
        &document.snapshot(),
        DisplayConfig::wrapped(80).with_wrap(Some(
            WrapConfig::new(80)
                .max_segments_per_line(8)
                .max_line_scan_bytes(512),
        )),
    );
    map.visible_rows(DisplayViewport::new(0, viewport_lines))
        .len() as u64
}

pub fn search_document(text: &str) -> u64 {
    let document = Document::new(text);
    let session = SearchSession::new(&document.snapshot(), SearchQuery::new("value_"));
    session.matches().len() as u64
}

pub fn folding_display_prepare(text: &str) -> u64 {
    let document = Document::new(text);
    let line_count = text.lines().count();
    let folds = if line_count > 4 {
        FoldSet::new([FoldRegion::new(0, (line_count / 2).min(line_count - 1))])
    } else {
        FoldSet::new([])
    };
    DisplayMap::with_folds(&document.snapshot(), DisplayConfig::unwrapped(), folds).row_count()
        as u64
}

pub fn diff_document(text: &str) -> u64 {
    let original = Document::new(text);
    let mut modified = text.to_owned();
    let offset = modified.len() / 2;
    modified.insert_str(offset, "// diff\n");
    compute_diff(&original.snapshot(), &Document::new(modified).snapshot())
        .hunks()
        .len() as u64
}

pub fn lifecycle_document(text: &str) -> u64 {
    let mut session = DocumentSession::new(
        Some("bench://document".into()),
        text,
        DocumentMetadata::detect_utf8(text),
    )
    .expect("generated text is valid UTF-8");
    session.apply_edit(0..0, "// lifecycle\n").unwrap();
    let request = session.request_save().unwrap();
    let completion = session.complete_save(
        request.id,
        mockaco_core::HostSaveResult::Success {
            location: "bench://saved".into(),
        },
    );
    match completion {
        mockaco_core::SaveCompletion::Saved(receipt) => receipt.event().text.len() as u64,
        _ => 0,
    }
}

pub fn workload_digest(text: &str, viewport_lines: usize) -> u64 {
    open_snapshot(text)
        ^ edit_transaction_mapping(text)
        ^ viewport_display_prepare(text, viewport_lines)
        ^ search_document(text)
        ^ folding_display_prepare(text)
        ^ diff_document(text)
        ^ lifecycle_document(text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    pub operation: &'static str,
    pub iterations: usize,
    pub total_nanos: u128,
    pub average_nanos: u128,
    pub digest: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkReport {
    pub implementation: &'static str,
    pub case: CaseSpec,
    pub metadata: DocumentMetadataSummary,
    pub viewport_lines: usize,
    pub warmup: usize,
    pub measurements: Vec<Measurement>,
}

fn timed<F>(
    operation: &'static str,
    text: &str,
    warmup: usize,
    iterations: usize,
    operation_fn: F,
) -> Measurement
where
    F: Fn(&str) -> u64,
{
    for _ in 0..warmup {
        std::hint::black_box(operation_fn(text));
    }
    let start = Instant::now();
    let mut digest: u64 = 0;
    for _ in 0..iterations {
        digest = digest.wrapping_add(std::hint::black_box(operation_fn(text)));
    }
    let total_nanos = start.elapsed().as_nanos();
    Measurement {
        operation,
        iterations,
        total_nanos,
        average_nanos: total_nanos / iterations as u128,
        digest,
    }
}

pub fn measure_case(
    case: CaseSpec,
    viewport_lines: usize,
    warmup: usize,
    iterations: usize,
) -> BenchmarkReport {
    assert!(iterations > 0, "benchmark iterations must be non-zero");
    let text = document_for(case);
    let metadata = metadata(&text);
    let viewport = viewport_lines.max(1);
    let mut measurements = vec![
        timed("open_snapshot", &text, warmup, iterations, open_snapshot),
        timed(
            "edit_transaction_mapping",
            &text,
            warmup,
            iterations,
            edit_transaction_mapping,
        ),
        timed("search", &text, warmup, iterations, search_document),
        timed(
            "folding_display_prepare",
            &text,
            warmup,
            iterations,
            folding_display_prepare,
        ),
        timed("diff", &text, warmup, iterations, diff_document),
        timed("lifecycle", &text, warmup, iterations, lifecycle_document),
    ];
    measurements.insert(
        2,
        timed(
            "viewport_display_prepare",
            &text,
            warmup,
            iterations,
            |value| viewport_display_prepare(value, viewport),
        ),
    );
    BenchmarkReport {
        implementation: "mockaco",
        case,
        metadata,
        viewport_lines: viewport,
        warmup,
        measurements,
    }
}

pub fn json_report(report: &BenchmarkReport) -> String {
    let measurements = report
        .measurements
        .iter()
        .map(|measurement| {
            format!(
                "{{\"operation\":\"{}\",\"iterations\":{},\"total_nanos\":{},\"average_nanos\":{},\"digest\":{}}}",
                measurement.operation,
                measurement.iterations,
                measurement.total_nanos,
                measurement.average_nanos,
                measurement.digest
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"implementation\":\"{}\",\"case\":\"{}\",\"byte_count\":{},\"line_count\":{},\"max_line_bytes\":{},\"sha256\":\"{}\",\"viewport_lines\":{},\"warmup\":{},\"measurements\":[{}]}}",
        report.implementation,
        report.case,
        report.metadata.byte_count,
        report.metadata.line_count,
        report.metadata.max_line_bytes,
        report.metadata.sha256,
        report.viewport_lines,
        report.warmup,
        measurements
    )
}

pub fn smoke_duration(report: &BenchmarkReport) -> Duration {
    Duration::from_nanos(
        report
            .measurements
            .iter()
            .map(|measurement| measurement.total_nanos as u64)
            .sum(),
    )
}
