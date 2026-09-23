#[cfg(feature = "native-wgpui")]
fn main() {
    mockaco_example::run();
}

#[cfg(not(feature = "native-wgpui"))]
fn main() {
    eprintln!("Run the native showcase with: cargo run -p mockaco-example --features native-wgpui");
}
