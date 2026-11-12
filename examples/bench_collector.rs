//! CPU cost of collector rounds: `cargo run --release --example bench_collector [pane]`.

use std::time::Instant;

use claude_tower::collector::Collector;
use claude_tower::config::Config;

fn cpu_ms() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let rest = &stat[stat.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = rest.split(' ').collect();
    // utime and stime are fields 14 and 15, so 11 and 12 after the name.
    let ticks: f64 = f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap();
    ticks * 10.0
}

fn main() {
    let pane = std::env::args().nth(1);
    let mut c = Collector::new(&Config::load().unwrap());
    c.collect(pane.as_deref());
    let (cpu0, t0) = (cpu_ms(), Instant::now());
    let rounds = 50;
    for _ in 0..rounds {
        c.collect(pane.as_deref());
    }
    let cpu = cpu_ms() - cpu0;
    println!(
        "{rounds} rounds: {:.1} ms wall, {:.2} ms CPU per round (10 ms tick resolution)",
        t0.elapsed().as_secs_f64() * 1000.0 / rounds as f64,
        cpu / rounds as f64
    );
}
