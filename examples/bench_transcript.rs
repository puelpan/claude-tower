//! Times the first full read of transcripts.
//!
//! `cargo run --release --example bench_transcript -- <file.jsonl>...`
//!
//! Prints per file: size, lines, ms, and which digest fields were found.
//! Never prints their text (§13).

use std::path::PathBuf;
use std::time::Instant;

use claude_tower::transcript::Reader;

const RUNS: u32 = 5;

fn main() {
    for path in std::env::args().skip(1).map(PathBuf::from) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                continue;
            }
        };
        let lines = bytes.iter().filter(|&&b| b == b'\n').count();

        // Best of a few runs, file in page cache.
        let mut best = f64::MAX;
        let mut reader = Reader::new(path.clone());
        for _ in 0..RUNS {
            reader = Reader::new(path.clone());
            let start = Instant::now();
            if let Err(e) = reader.update() {
                eprintln!("{}: {e}", path.display());
            }
            best = best.min(start.elapsed().as_secs_f64() * 1000.0);
        }

        let d = reader.digest();
        let found = [
            ("custom_title", d.custom_title.is_some()),
            ("ai_title", d.ai_title.is_some()),
            ("recap", d.recap.is_some()),
            ("last_prompt", d.last_prompt.is_some()),
            ("last_said", d.last_said.is_some()),
            ("pr", d.pr.is_some()),
            ("permission_mode", d.permission_mode.is_some()),
            ("git_branch", d.git_branch.is_some()),
            ("last_activity", d.last_activity_ms.is_some()),
        ];
        let found: Vec<&str> = found.iter().filter(|f| f.1).map(|f| f.0).collect();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        println!(
            "{:.8}  {:5.2} MB  {:5} lines  {:6.2} ms  {}",
            name,
            bytes.len() as f64 / 1e6,
            lines,
            best,
            found.join(" ")
        );
    }
}
