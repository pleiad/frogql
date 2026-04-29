//! LDBC SNB Interactive workload benchmark runner.
//!
//! Section 6 of the LDBC SNB v1 paper (arXiv:2001.02299) defines the
//! Interactive Complex (IC) reads. This binary runs the subset of those
//! queries that gqlite supports today, measuring runtime per query.
//!
//! Usage:
//!     ldbc_bench <db.gdb> [--iters N] [--limit N]
//!
//! The DB is built from the LDBC SF0.1 CsvBasic dataset via:
//!     gqlite db.gdb --import-ldbc-csv <path-to-extracted-dataset>
//!
//! Per LDBC methodology, each IC is parameterized — we sweep a small
//! curated parameter set per query and report min / median / max wall
//! time across (params × iters) runs.
//!
//! Currently supported: **IC2** (recent messages by friends).
//!
//! ### IC2 fidelity vs the spec
//!
//! Spec text (LDBC SNB v1 §6 IC2):
//!     MATCH (:Person {id: $personId})-[:KNOWS]-(friend:Person)
//!           <-[:HAS_CREATOR]-(message:Message)
//!     WHERE message.creationDate <= $maxDate
//!     RETURN friend.id, friend.firstName, friend.lastName,
//!            message.id, coalesce(message.content, message.imageFile),
//!            message.creationDate
//!     ORDER BY creationDate DESC, id ASC
//!     LIMIT 20
//!
//! Divergences this bench keeps:
//!
//! - **Anchor by `(firstName, lastName)` pair instead of `id`.** The
//!   gqlite LDBC loader folds the LDBC `id` column into the internal
//!   *node name*, not into a property, so `Person.id = $personId` is
//!   not addressable. Each parameter below is a `(firstName, lastName)`
//!   pair that uniquely identifies one Person in SF0.1 — same per-query
//!   selectivity as `id`, just spelled differently.
//!
//! - **No ORDER BY.** gqlite's parser does not have ORDER BY yet.
//!   Output order is whatever the runtime emits. The `limit` argument
//!   still caps at 20 rows, but those 20 are not guaranteed to be the
//!   20 most recent. Wall time is unaffected by sort.
//!
//! - **Drop `friend.id` / `message.id` from RETURN.** Same loader
//!   reason as above — both would render as `"NULL"` rather than the
//!   spec's IDs.
//!
//! - **`coalesce(message.content, message.imageFile)` collapsed to
//!   `c.content`.** Posts in SF0.1 mostly have non-empty `content`;
//!   image-only posts will return blank content. gqlite has no
//!   `coalesce` builtin.
//!
//! Spec features matched:
//! - **`Message = Comment ∪ Post`** via path-pattern union (`|`). Both
//!   `Comment` and `Post` arms are checked in a single query; the
//!   `c` variable binds to whichever matched.
//! - **`message.creationDate <= $maxDate`** — direct numeric WHERE
//!   predicate. `maxDate` here is mid-2012 (1 340 000 000 000 ms),
//!   chosen to retain enough rows that the join still does work but
//!   exercise the filter on creationDate.
//! - **`LIMIT 20`** — passed via `Runtime::run_query`'s `limit`.
//!
//! Other ICs need features gqlite doesn't yet have:
//!   - IC1 (shortest paths, OPTIONAL MATCH, complex aggregation)
//!   - IC3-IC8, IC10-IC14 (transitive paths, OPTIONAL MATCH, date
//!     arithmetic, aggregate-with-HAVING, etc.)
//!
//! Typechecking is skipped — IC queries are well-formed by design,
//! and bench timing should reflect runtime dominance, not checker work.

use std::env;
use std::path::Path;
use std::time::{Duration, Instant};

use gqlrust::compile_query_unchecked;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;

/// Spec-fidelity anchor: `(firstName, lastName)` pairs that each map
/// to exactly one Person in SF0.1, replacing `Person.id = $personId`.
const PARAMS: &[(&str, &str)] = &[
    ("Mahinda", "Perera"),
    ("Carmen", "Lepland"),
    ("Bryn", "Davies"),
    ("Cheng", "Yu"),
    ("Hồ Chí", "Loan"),
];

/// LDBC IC2's `$maxDate`. Mid-2012 in ms-since-epoch — chosen so the
/// filter retains a sizable fraction of comments / posts in SF0.1
/// (whose creationDates span roughly 2010-2013) without trivially
/// matching every row. The spec's example value (1 287 230 400 000 =
/// 2010-10-16) is keyed to a larger SF and would cut all SF0.1 data.
const MAX_DATE_MS: i64 = 1_340_000_000_000;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <db.gdb> [--iters N] [--limit N]", args[0]);
        std::process::exit(1);
    }
    let db_path = &args[1];

    let mut iters: usize = 5;
    let mut limit: usize = 20; // matches IC2's `LIMIT 20`
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => {
                iters = args[i + 1].parse().expect("invalid iters");
                i += 2;
            }
            "--limit" => {
                limit = args[i + 1].parse().expect("invalid limit");
                i += 2;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(1);
            }
        }
    }

    eprintln!("Loading {db_path}...");
    let t0 = Instant::now();
    let store = LazyGraphStore::open(Path::new(db_path)).expect("open .gdb");
    eprintln!(
        "  loaded {} nodes / {} edges in {:.2}s",
        store.node_count(),
        store.edge_count(),
        t0.elapsed().as_secs_f64()
    );
    let rt = Runtime::new(&store);

    eprintln!("\n=== IC2: Recent messages by friends ===");
    eprintln!(
        "Anchor: (firstName, lastName) per LDBC `personId` semantics; \
         maxDate <= {MAX_DATE_MS}; {iters} iters/param; limit={limit}"
    );
    println!("query;first_name;last_name;iter;result_count;elapsed_ns");

    for (first_name, last_name) in PARAMS {
        // Path-pattern union covers spec's `(message:Message)` =
        // `Comment ∪ Post`. `c` binds to whichever arm matched.
        let q = format!(
            "MATCH (p: Person)~[:knows]~(friend: Person)\
             <-[:hasCreator]-(c: Comment) | \
             (p: Person)~[:knows]~(friend: Person)\
             <-[:hasCreator]-(c: Post) \
             WHERE p.firstName = '{first_name}' \
             AND p.lastName = '{last_name}' \
             AND c.creationDate <= {MAX_DATE_MS} \
             RETURN friend.firstName, friend.lastName, \
             c.content, c.creationDate"
        );
        let parsed = match compile_query_unchecked(&q) {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!("  PARSE ERROR for {first_name} {last_name}: {e}");
                continue;
            }
        };

        let mut samples: Vec<Duration> = Vec::with_capacity(iters);
        let mut last_count = 0usize;
        for n in 0..iters {
            let start = Instant::now();
            let result = rt.run_query(&parsed, limit);
            let elapsed = start.elapsed();
            samples.push(elapsed);
            last_count = result.row_count();
            println!(
                "IC2;{first_name};{last_name};{n};{};{}",
                last_count,
                elapsed.as_nanos()
            );
        }
        report("IC2", first_name, last_name, &samples, last_count);
    }
}

fn report(query: &str, first_name: &str, last_name: &str, samples: &[Duration], count: usize) {
    let mut sorted: Vec<u128> = samples.iter().map(|d| d.as_nanos()).collect();
    sorted.sort_unstable();
    let n = sorted.len();
    if n == 0 {
        return;
    }
    let min = sorted[0];
    let max = sorted[n - 1];
    let median = sorted[n / 2];
    let mean = sorted.iter().sum::<u128>() / n as u128;
    eprintln!(
        "  {query} ({first_name} {last_name:<10}) count={count:<3} \
         min={:>8.2}ms  med={:>8.2}ms  mean={:>8.2}ms  max={:>8.2}ms",
        min as f64 / 1e6,
        median as f64 / 1e6,
        mean as f64 / 1e6,
        max as f64 / 1e6,
    );
}
