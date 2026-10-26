//! Time the store on a real phext.
//!
//! ```text
//! cargo run --release --example bench -- path/to/big.phext
//! ```
//!
//! Works on a copy next to the system temp dir, so the input is not changed.

use std::time::{Duration, Instant};

use chrono::Utc;
use lanes::coord::{parse_coord, Target};
use lanes::store::{Post, Store};

fn time<T>(label: &str, runs: u32, mut body: impl FnMut() -> T) -> T {
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    let mut last = None;
    for _ in 0..runs {
        let start = Instant::now();
        last = Some(body());
        let spent = start.elapsed();
        best = best.min(spent);
        total += spent;
    }
    println!(
        "{label:<28} best {:>9.3} ms   mean {:>9.3} ms   ({runs} runs)",
        best.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0 / f64::from(runs)
    );
    last.expect("at least one run")
}

/// Compare the linear index with libphext on a real file. Before libphext
/// 0.3.2 this was quadratic; with 0.3.2 it handles multi-megabyte files.
fn check(input: &str) {
    let doc = std::fs::read_to_string(input).expect("read");
    let ours: std::collections::HashMap<_, _> = lanes::scrolls::tokenize(&doc)
        .into_iter()
        .map(|(coord, range)| (coord, doc[range].to_string()))
        .collect();
    let theirs: std::collections::HashMap<_, _> = libphext::phext::explode(&doc)
        .into_iter()
        .filter(|(_, text)| !text.is_empty())
        .collect();
    assert_eq!(ours, theirs, "{input}: scroll maps differ");
    let rebuilt = lanes::scrolls::serialize({
        let mut sorted: Vec<_> = ours.iter().map(|(c, t)| (*c, t.as_str())).collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        sorted
    });
    assert_eq!(
        rebuilt,
        libphext::phext::implode(libphext::phext::explode(&doc)),
        "{input}: serialized documents differ"
    );
    println!("{input}: {} scrolls match libphext", ours.len());
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--check") {
        for input in &args[1..] {
            check(input);
        }
        return;
    }
    let input = args.pop().expect("usage: bench [--check] <file.phext>...");
    let copy = std::env::temp_dir().join(format!("lanes-bench-{}.phext", std::process::id()));
    std::fs::copy(&input, &copy).expect("copy input");
    let bytes = std::fs::metadata(&copy).expect("stat").len();
    println!("{input}: {:.2} MB", bytes as f64 / 1_048_576.0);

    let store = time("open (read + index)", 5, || {
        Store::open(&copy).expect("open")
    });
    let world = store.world();
    let messages: usize = world.threads.iter().map(|t| t.messages.len()).sum();
    println!(
        "{} threads, {} scrolls in threads, {} readers",
        world.threads.len(),
        messages,
        world.users.len()
    );
    let probe = world
        .threads
        .last()
        .and_then(|thread| thread.messages.last())
        .map(|message| message.coord)
        .unwrap_or_else(|| parse_coord("1.1.1/1.1.1/1.1.1").unwrap());
    time("select one scroll", 50, || store.select(probe).len());
    time("toc (textmap)", 5, || store.toc().len());
    time("doc for download", 50, || store.doc().len());

    let mut store = store;
    let target = parse_coord("9.9.9/9.9.9/1.1.1").unwrap();
    time("place one message", 20, || {
        store
            .place(Post {
                author: "bench".into(),
                who: String::new(),
                text: "a benchmark note".into(),
                target: Target::Thread(target),
                now: Utc::now(),
            })
            .expect("place")
    });
    drop(store);
    let _ = std::fs::remove_file(&copy);
}
