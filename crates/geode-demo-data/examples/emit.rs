//! Write a demo source directory: `cargo run -p geode-demo-data --example emit -- <dir> [rows]`
//!
//! Exists so the throwaway data probe (spec §7, `geode-app/src/probe.rs`)
//! can be pointed at something real without a live desk share. The
//! generator is deterministic, so the same seed gives the same files.

use geode_demo_data::{EmitOptions, GeneratorConfig, emit_directory, generate};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: emit <dir> [rows]");
        std::process::exit(2);
    };
    let rows: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(100_000);

    std::fs::create_dir_all(&dir).expect("creating the output directory");
    let batch = generate(&GeneratorConfig {
        rows,
        seed: 42,
        business_dates: 1,
    });
    let emitted =
        emit_directory(&batch, &EmitOptions::new(std::path::Path::new(&dir))).expect("emitting");
    let ready = emitted
        .files
        .iter()
        .filter(|f| f.sentinel_path.is_some())
        .count();
    println!(
        "{} files ({ready} with sentinels), {} rows into {dir}",
        emitted.files.len(),
        emitted.files.iter().map(|f| f.rows).sum::<usize>(),
    );
}
