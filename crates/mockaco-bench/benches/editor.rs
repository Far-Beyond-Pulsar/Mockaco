use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mockaco_bench::{
    diff_document, document_for, edit_transaction_mapping, folding_display_prepare,
    lifecycle_document, open_snapshot, search_document, viewport_display_prepare, CaseSpec,
};

fn editor_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("mockaco-editor");
    group.sample_size(10);
    for case in CaseSpec::ALL {
        let text = document_for(case);
        let name = case.name;
        group.bench_function(format!("open_snapshot/{name}"), |b| {
            b.iter(|| open_snapshot(black_box(&text)))
        });
        group.bench_function(format!("edit_transaction_mapping/{name}"), |b| {
            b.iter(|| edit_transaction_mapping(black_box(&text)))
        });
        group.bench_function(format!("viewport_display_prepare/{name}"), |b| {
            b.iter(|| viewport_display_prepare(black_box(&text), 60))
        });
        group.bench_function(format!("search/{name}"), |b| {
            b.iter(|| search_document(black_box(&text)))
        });
        group.bench_function(format!("folding_display_prepare/{name}"), |b| {
            b.iter(|| folding_display_prepare(black_box(&text)))
        });
        group.bench_function(format!("diff/{name}"), |b| {
            b.iter(|| diff_document(black_box(&text)))
        });
        group.bench_function(format!("lifecycle/{name}"), |b| {
            b.iter(|| lifecycle_document(black_box(&text)))
        });
    }
    group.finish();
}

criterion_group!(benches, editor_workloads);
criterion_main!(benches);
