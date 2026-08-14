//! Dev-only benchmark: `cargo run --release --example bench_import -- <path>`
//! Measures import time and LOD-query time against a large log file.

use std::path::Path;
use std::time::Instant;

use rapid_analyzer::model::SourceKind;

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench_import <path>");
    let path = Path::new(&path);

    let file_size = std::fs::metadata(path).unwrap().len();
    println!("importing {} ({:.1} MB)", path.display(), file_size as f64 / 1e6);

    let t0 = Instant::now();
    let (_name, kind) = rapid_analyzer::import::import_path(path).expect("import failed");
    let import_time = t0.elapsed();

    let SourceKind::Log(log) = kind else {
        panic!("expected a log source");
    };
    println!("imported {} series in {:?}", log.series.len(), import_time);

    let total_points: usize = log
        .series
        .iter()
        .map(|s| s.time_bounds().is_some() as usize)
        .sum();
    println!("series with data: {total_points}");

    let Some((lo, hi)) = log
        .series
        .iter()
        .filter_map(|s| s.time_bounds())
        .fold(None, |acc, (a, b)| {
            Some(match acc {
                Some((l, h)) => (f64::min(l, a), f64::max(h, b)),
                None => (a, b),
            })
        })
    else {
        println!("no data");
        return;
    };
    println!("time span: {:.1}s", hi - lo);

    // Simulate ~60 frames of rendering at full zoom-out (worst case: every
    // series' whole range queried every frame, as the UI does per pane).
    let t1 = Instant::now();
    let frames = 60;
    for _ in 0..frames {
        for s in &log.series {
            let pts = s.slice_for_range(lo, hi, 0.0, 2000);
            std::hint::black_box(&pts);
        }
    }
    let query_time = t1.elapsed();
    println!(
        "{frames} frames x {} series full-range queries: {:?} total, {:?}/frame",
        log.series.len(),
        query_time,
        query_time / frames
    );

    // And a fully zoomed-in query (1 second window).
    let t2 = Instant::now();
    for _ in 0..frames {
        for s in &log.series {
            let pts = s.slice_for_range(lo, lo + 1.0, 0.0, 2000);
            std::hint::black_box(&pts);
        }
    }
    let zoom_query_time = t2.elapsed();
    println!(
        "{frames} frames x {} series 1s-window queries: {:?} total, {:?}/frame",
        log.series.len(),
        zoom_query_time,
        zoom_query_time / frames
    );
}
