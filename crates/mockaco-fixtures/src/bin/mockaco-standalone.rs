use mockaco_fixtures::run_standalone;

fn main() {
    let report = run_standalone();
    println!("fixtures={}", report.fixture_count);
    println!("document_bytes={}", report.document_bytes);
    println!("document_version={}", report.final_document_version);
    println!("render_rows={}", report.render_rows);
    println!("diff_hunks={}", report.diff_hunks);
    println!("save_event_bytes={}", report.save_event_text.len());
}
