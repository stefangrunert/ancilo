//! Timing of `results::shorten` on a 2 MB log (FPL-02 resource budget).
use ancilo_agent::compress;
use ancilo_agent::results::{Meta, shorten};
use std::time::Instant;

fn main() {
    let log: String = (0..60_000)
        .map(|i| {
            if i % 997 == 0 {
                format!("case {i} FAILED: x\n")
            } else {
                format!("case {i} ok – fine\n")
            }
        })
        .collect();
    let meta = Meta {
        tool: "bash".into(),
        error: true,
        id: Some("r1".into()),
    };
    let t = Instant::now();
    let _ = compress::summarise_log(&log);
    println!("summarise_log {:?}", t.elapsed());
    for budget in [20_000, 2000, 800, 150] {
        let t = Instant::now();
        let s = shorten(&meta, &log, budget);
        println!(
            "shorten {budget}: {:?} ({} chars)",
            t.elapsed(),
            s.chars().count()
        );
    }
}
