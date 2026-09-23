use mockaco_bench::{json_report, measure_case, CaseSpec};
use std::env;
use std::fs;
use std::path::Path;

fn value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn parse_usize(args: &[String], name: &str, default: usize) -> usize {
    value(args, name)
        .map(|value| {
            value
                .parse()
                .unwrap_or_else(|_| panic!("invalid {name}: {value}"))
        })
        .unwrap_or(default)
}

fn main() {
    let args = env::args().collect::<Vec<_>>();
    let implementation = value(&args, "--implementation").unwrap_or_else(|| "mockaco".into());
    if implementation != "mockaco" {
        eprintln!("implementation {implementation:?} is unavailable: legacy sources are not in this checkout");
        std::process::exit(2);
    }
    let case_name = value(&args, "--case").unwrap_or_else(|| "1k".into());
    let case = CaseSpec::parse(&case_name).unwrap_or_else(|| {
        panic!("unknown case {case_name:?}; choose 1k, 100k, 1m, or pathological-line")
    });
    let viewport = parse_usize(&args, "--viewport-lines", 60);
    let warmup = parse_usize(&args, "--warmup", 2);
    let iterations = parse_usize(&args, "--iterations", 10).max(1);
    let report = measure_case(case, viewport, warmup, iterations);
    let json = json_report(&report);
    if let Some(path) = value(&args, "--json") {
        if let Some(parent) = Path::new(&path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).unwrap_or_else(|error| {
                panic!("failed to create benchmark directory {parent:?}: {error}")
            });
        }
        fs::write(&path, format!("{json}\n"))
            .unwrap_or_else(|error| panic!("failed to write benchmark report {path:?}: {error}"));
    }
    println!("{json}");
}
