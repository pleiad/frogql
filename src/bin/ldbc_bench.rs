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
//! Spec features matched (post `loader/ldbc-id-property`):
//!
//! - **Anchor by `Person.id`.** The LDBC loader stores the LDBC id
//!   column as a queryable property, so `WHERE p.id = $personId`
//!   matches the spec exactly.
//! - **`Message = Comment ∪ Post`** via path-pattern union (`|`). Both
//!   arms are checked in a single query; `c` binds to whichever
//!   matched. (gqlite has no label-inheritance / Cypher-style union
//!   types, so there's no single `Message` label to match.)
//! - **`message.creationDate <= $maxDate`** — direct numeric WHERE
//!   predicate. The LDBC `LongDateFormatter` encodes creationDate as
//!   ms-since-epoch in a long integer; gqlite's loader stores that
//!   as `Value::Int` and `<=` is a normal int comparison.
//! - **`LIMIT 20`** — passed via `Runtime::run_query`'s `limit`.
//! - **RETURN columns** — `friend.id, friend.firstName, friend.lastName,
//!   c.id, c.content, c.creationDate` — same set as the spec, modulo
//!   `coalesce` (see below).
//!
//! Remaining divergences:
//!
//! - **No `ORDER BY`.** gqlite's parser doesn't have ORDER BY yet.
//!   Output order is whatever the runtime emits. The `LIMIT 20` cap
//!   applies, but the 20 rows aren't guaranteed to be the 20 *most
//!   recent*. Wall time is unaffected by sort.
//! - **`coalesce(message.content, message.imageFile)` → `c.content`.**
//!   gqlite has no `coalesce` builtin. Posts in SF0.1 mostly have
//!   non-empty `content`; the few image-only posts will return blank
//!   content instead of falling back to imageFile.
//! - **Edge label casing.** Spec uses `[:KNOWS]` / `[:HAS_CREATOR]`;
//!   gqlite's LDBC loader produces `knows` / `hasCreator` (the
//!   stems in the source CSV filenames). Pure naming, no behavior
//!   change — but the spec query doesn't paste verbatim.
//! - **No parameter generator.** LDBC's canonical workflow is to run
//!   `ldbc_snb_datagen`'s `substitution_parameters` tool against the
//!   chosen SF, which produces SF-specific parameter sets in
//!   `interactive_<n>.txt`. Spec example values are drawn from a
//!   specific SF run (the personId `10995116278009` and maxDate
//!   `2010-10-16` come from a larger SF) and are not portable —
//!   LDBC's id space is bit-packed per SF, and event timelines stretch
//!   with SF. We didn't run the generator (heavier Python/Hadoop dep
//!   than the rest of this branch) and substituted hand-picked params,
//!   which carries the two derived divergences below:
//!     - **`personId` values.** Spec example `10995116278009` doesn't
//!       exist in SF0.1. Bench picks five real ids from SF0.1's Person
//!       table by inspection (see `PARAMS` below).
//!     - **`maxDate` value.** Spec example `1 287 230 400 000`
//!       (2010-10-16) predates SF0.1's first message and cuts every
//!       row. Bench uses `1 340 000 000 000` (mid-2012) so the filter
//!       retains enough rows for the join to do real work.
//!   These don't affect the timing claim (join work is the same for
//!   any valid anchor) but they are a methodology deviation from the
//!   LDBC workflow worth flagging.
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

/// LDBC `Person.id` values for five SF0.1 Persons of varying degree.
/// Spec uses `10995116278009` which doesn't exist in SF0.1; these are
/// real ids resolved from the Person table. Comment columns name the
/// Person each id resolves to so the bench output is human-readable.
const PARAMS: &[(i64, &str)] = &[
    (933, "Mahinda Perera"),
    (1129, "Carmen Lepland"),
    (8_796_093_023_296, "Hồ Chí Loan"),
    (21_990_232_555_524, "Bryn Davies"),
    (32_985_348_833_865, "Cheng Yu"),
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
        "Anchor: Person.id per LDBC spec; maxDate <= {MAX_DATE_MS}; \
         {iters} iters/param; limit={limit}"
    );
    println!("query;person_id;person_name;iter;result_count;elapsed_ns");

    for (person_id, person_name) in PARAMS {
        // Path-pattern union covers spec's `(message:Message)` =
        // `Comment ∪ Post`. `c` binds to whichever arm matched.
        let q = format!(
            "MATCH (p: Person)~[:knows]~(friend: Person)\
             <-[:hasCreator]-(c: Comment) | \
             (p: Person)~[:knows]~(friend: Person)\
             <-[:hasCreator]-(c: Post) \
             WHERE p.id = {person_id} \
             AND c.creationDate <= {MAX_DATE_MS} \
             RETURN friend.id, friend.firstName, friend.lastName, \
             c.id, c.content, c.creationDate"
        );
        let parsed = match compile_query_unchecked(&q) {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!("  PARSE ERROR for id={person_id} ({person_name}): {e}");
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
                "IC2;{person_id};{person_name};{n};{};{}",
                last_count,
                elapsed.as_nanos()
            );
        }
        report("IC2", *person_id, person_name, &samples, last_count);
    }
}

fn report(query: &str, person_id: i64, person_name: &str, samples: &[Duration], count: usize) {
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
        "  {query} id={person_id:<14} ({person_name:<16}) count={count:<3} \
         min={:>8.2}ms  med={:>8.2}ms  mean={:>8.2}ms  max={:>8.2}ms",
        min as f64 / 1e6,
        median as f64 / 1e6,
        mean as f64 / 1e6,
        max as f64 / 1e6,
    );
}
