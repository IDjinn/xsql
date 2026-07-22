//! Benchmarks the two XML parse paths (quick-xml streaming vs simdxml
//! structural index) over real files and verifies both produce the same DOM.
//!
//! Usage: xsql-bench [--iters N] [--comments] <file.xml> [more.xml ...]
//!
//! Without --iters the iteration count is chosen per file so each parser runs
//! for roughly one second (clamped to 5..=500 iterations).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use xsql::xml::dom::Document;
use xsql::xml::{parse, parse_simd};

const TARGET_TIME: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let mut iters_override: Option<u32> = None;
    let mut keep_comments = false;
    let mut files: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iters" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => iters_override = Some(n),
                None => {
                    eprintln!("--iters requires a number");
                    return ExitCode::FAILURE;
                }
            },
            "--comments" => keep_comments = true,
            "--help" | "-h" => {
                eprintln!("usage: xsql-bench [--iters N] [--comments] <file.xml> ...");
                return ExitCode::SUCCESS;
            }
            _ => files.push(arg),
        }
    }

    if files.is_empty() {
        eprintln!("usage: xsql-bench [--iters N] [--comments] <file.xml> ...");
        return ExitCode::FAILURE;
    }

    println!(
        "{:<28} {:>9} {:>6} {:>12} {:>12} {:>8}  {}",
        "file", "size", "iters", "quick-xml", "simdxml", "speedup", "doms"
    );

    let mut ok = true;
    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{path}: {e}");
                ok = false;
                continue;
            }
        };

        let quick = parse::parse_document_opts(&source, keep_comments);
        let simd = parse_simd::parse_document_opts(&source, keep_comments);
        let (quick_doc, simd_doc) = match (quick, simd) {
            (Ok(q), Ok(s)) => (q, s),
            (q, s) => {
                if let Err(e) = q {
                    eprintln!("{path}: quick-xml parse failed: {e}");
                }
                if let Err(e) = s {
                    eprintln!("{path}: simdxml parse failed: {e}");
                }
                ok = false;
                continue;
            }
        };

        let doms_match = docs_equal(&quick_doc, &simd_doc);
        if !doms_match {
            ok = false;
        }

        let iters = iters_override.unwrap_or_else(|| calibrate(&source, keep_comments));

        let t_quick = bench(iters, || {
            parse::parse_document_opts(&source, keep_comments).unwrap()
        });
        let t_simd = bench(iters, || {
            parse_simd::parse_document_opts(&source, keep_comments).unwrap()
        });

        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        println!(
            "{:<28} {:>9} {:>6} {:>12} {:>12} {:>7.2}x  {}",
            name,
            human_size(source.len()),
            iters,
            format_run(t_quick, source.len()),
            format_run(t_simd, source.len()),
            t_quick.as_secs_f64() / t_simd.as_secs_f64(),
            if doms_match { "match" } else { "MISMATCH" },
        );
        if !doms_match {
            report_first_diff(&quick_doc, &simd_doc);
        }
    }

    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Median wall time of `iters` runs of `f`.
fn bench<T>(iters: u32, mut f: impl FnMut() -> T) -> Duration {
    for _ in 0..iters.min(3) {
        std::hint::black_box(f());
    }
    let mut times: Vec<Duration> = (0..iters)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(f());
            start.elapsed()
        })
        .collect();
    times.sort();
    times[times.len() / 2]
}

fn calibrate(source: &str, keep_comments: bool) -> u32 {
    let start = Instant::now();
    std::hint::black_box(parse::parse_document_opts(source, keep_comments).unwrap());
    let once = start.elapsed().max(Duration::from_micros(1));
    (TARGET_TIME.as_secs_f64() / once.as_secs_f64()).round().clamp(5.0, 500.0) as u32
}

fn format_run(t: Duration, bytes: usize) -> String {
    let mbps = bytes as f64 / 1e6 / t.as_secs_f64();
    if t < Duration::from_millis(1) {
        format!("{:>5.0}us {mbps:>4.0}M/s", t.as_secs_f64() * 1e6)
    } else {
        format!("{:>5.1}ms {mbps:>4.0}M/s", t.as_secs_f64() * 1e3)
    }
}

fn human_size(bytes: usize) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1}MB", bytes as f64 / 1e6)
    } else {
        format!("{:.1}KB", bytes as f64 / 1e3)
    }
}

/// Structural DOM comparison, independent of arena id assignment order.
fn docs_equal(a: &Document, b: &Document) -> bool {
    a.had_decl == b.had_decl
        && a.roots.len() == b.roots.len()
        && a.roots
            .iter()
            .zip(&b.roots)
            .all(|(&x, &y)| subtree_equal(a, x, b, y))
}

fn subtree_equal(a: &Document, ai: usize, b: &Document, bi: usize) -> bool {
    let ea = a.node(ai);
    let eb = b.node(bi);
    ea.tag == eb.tag
        && ea.attrs == eb.attrs
        && ea.text == eb.text
        && ea.children.len() == eb.children.len()
        && ea
            .children
            .iter()
            .zip(&eb.children)
            .all(|(&x, &y)| subtree_equal(a, x, b, y))
}

fn report_first_diff(a: &Document, b: &Document) {
    for (i, (&x, &y)) in a.roots.iter().zip(&b.roots).enumerate() {
        if let Some(msg) = first_diff(a, x, b, y, format!("root[{i}]")) {
            eprintln!("  first difference: {msg}");
            return;
        }
    }
    eprintln!(
        "  root count differs: quick-xml {} vs simdxml {}",
        a.roots.len(),
        b.roots.len()
    );
}

fn first_diff(a: &Document, ai: usize, b: &Document, bi: usize, path: String) -> Option<String> {
    let ea = a.node(ai);
    let eb = b.node(bi);
    let path = format!("{path}/{}", ea.tag);
    if ea.tag != eb.tag {
        return Some(format!("{path}: tag {:?} vs {:?}", ea.tag, eb.tag));
    }
    if ea.attrs != eb.attrs {
        return Some(format!("{path}: attrs {:?} vs {:?}", ea.attrs, eb.attrs));
    }
    if ea.text != eb.text {
        return Some(format!("{path}: text {:?} vs {:?}", ea.text, eb.text));
    }
    if ea.children.len() != eb.children.len() {
        return Some(format!(
            "{path}: child count {} vs {}",
            ea.children.len(),
            eb.children.len()
        ));
    }
    ea.children
        .iter()
        .zip(&eb.children)
        .enumerate()
        .find_map(|(i, (&x, &y))| first_diff(a, x, b, y, format!("{path}[{i}]")))
}
