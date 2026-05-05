//! Load the LDBC SF0.1 IC2 subset into a GraphLite-AI/GraphLite Sled-backed
//! database. Mirrors the role of `bench/cross-system/graphqlite/setup.py` for
//! the GraphLite engine.
//!
//! IC2 only references these node/edge types:
//!   - Person nodes (id, firstName, lastName)
//!   - Comment nodes (id, creationDate, content)
//!   - Post nodes (id, creationDate, content)
//!   - knows edges (Person—Person; LDBC stores one direction, we insert both
//!     to simulate undirected matching)
//!   - hasCreator edges (Comment→Person, Post→Person)
//!
//! Output: a Sled directory at
//! `bench/data/cross-system/graphlite/ic<n>.db/` with the schema/graph created
//! and all rows loaded. Idempotent: skips if the directory already exists.

use clap::Parser;
use csv::ReaderBuilder;
use graphlite_sdk::GraphLite;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(about = "Load LDBC CSVs into a GraphLite Sled-backed DB.")]
struct Args {
    /// LDBC IC number this DB is for (used in the output dir name).
    #[arg(long, default_value_t = 2)]
    ic: u32,
    /// LDBC dynamic-CSVs directory (Person, Comment, Post + edge files).
    #[arg(long)]
    csv_dir: Option<PathBuf>,
    /// Output Sled directory.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Rebuild even if the directory already exists.
    #[arg(long)]
    force: bool,
}

fn repo_root() -> PathBuf {
    // setup.rs is at bench/cross-system/graphlite/src/setup.rs.
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    here.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
}

fn default_csv_dir(repo: &Path) -> PathBuf {
    repo.join("bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/dynamic")
}

fn default_db(repo: &Path, ic: u32) -> PathBuf {
    repo.join(format!("bench/data/cross-system/graphlite/ic{ic}.db"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repo = repo_root();
    let csv_dir = args.csv_dir.unwrap_or_else(|| default_csv_dir(&repo));
    let db_path = args.db.unwrap_or_else(|| default_db(&repo, args.ic));

    if args.ic != 2 {
        eprintln!(
            "setup currently only loads the IC2 subset (Person/Comment/Post + knows/hasCreator). \
             Add per-IC loaders before benching ic{}.",
            args.ic
        );
        std::process::exit(1);
    }

    if !csv_dir.is_dir() {
        eprintln!("CSV dir not found: {}", csv_dir.display());
        eprintln!("Run ./target/release/bench_setup from the repo root first.");
        std::process::exit(1);
    }

    if db_path.exists() {
        if args.force {
            fs::remove_dir_all(&db_path)?;
        } else {
            eprintln!("  cached: {} (pass --force to rebuild)", db_path.display());
            return Ok(());
        }
    }
    fs::create_dir_all(db_path.parent().unwrap())?;

    eprintln!("  building {} from {}", db_path.display(), csv_dir.display());
    let t0 = std::time::Instant::now();

    let db = GraphLite::open(&db_path)?;
    let session = db.session("admin")?;

    // GraphLite SDK 0.0.1: schema/graph context is set with SESSION SET,
    // not `USE` (which the parser rejects). Order matters — schema first,
    // graph second.
    session.execute("CREATE SCHEMA IF NOT EXISTS ldbc")?;
    session.execute("SESSION SET SCHEMA ldbc")?;
    session.execute("CREATE GRAPH IF NOT EXISTS sf01")?;
    session.execute("SESSION SET GRAPH sf01")?;

    load_persons(&session, &csv_dir.join("person_0_0.csv"))?;
    load_messages(&session, &csv_dir.join("comment_0_0.csv"), "Comment")?;
    load_messages(&session, &csv_dir.join("post_0_0.csv"), "Post")?;

    // knows is undirected per LDBC; the CSV lists each pair once. We
    // insert the edge in both directions so query patterns can match
    // either direction without needing native undirected-pattern syntax.
    load_edges_knows(&session, &csv_dir.join("person_knows_person_0_0.csv"))?;
    load_edges_has_creator(&session, &csv_dir.join("comment_hasCreator_person_0_0.csv"))?;
    load_edges_has_creator(&session, &csv_dir.join("post_hasCreator_person_0_0.csv"))?;

    eprintln!("  done in {:.1}s. db at {}", t0.elapsed().as_secs_f64(), db_path.display());
    Ok(())
}

/// Escape a string literal for inclusion in a GQL query: wrap in single quotes
/// and **backslash-escape** embedded single quotes and backslashes. LDBC strings
/// can contain apostrophes (e.g. names like "O'Brien", LDBC content text like
/// "BBC's") so this matters. GraphLite's lexer recognises only backslash escape
/// (`\'`) — NOT the SQL-style double-quote escape (`''`) — see
/// `graphlite-0.0.1/src/ast/lexer.rs::escaped_string_content`. Without this fix
/// the lexer terminates the string at the first inner quote and the remainder
/// of the INSERT becomes garbled, surfacing as `Parse error: UnexpectedToken(Insert)`
/// when the next statement begins.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => {
                out.push('\\');
                out.push('\\');
            }
            '\'' => {
                out.push('\\');
                out.push('\'');
            }
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Strip non-ASCII characters from `s`. GraphLite 0.0.1's lexer panics on
/// any non-ASCII byte that lands inside a multi-byte char boundary during
/// keyword detection (`src/ast/lexer.rs:488`, `&input[..N]` slicing without
/// `is_char_boundary` check). Until upstream fixes that, we drop non-ASCII
/// chars from string property values at load time. This affects content
/// fidelity for names like "Amenábar" → "Amenbar" but preserves IC2 row
/// counts and result shapes (IC2 doesn't filter on string content, only
/// returns it). See DIVERGENCES.md.
fn ascii_only(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii()).collect()
}

/// Run a closure that calls into the SDK, catching both `Result::Err` and
/// any `panic!` inside the SDK. Returns `Ok(())` on success and `Err(reason)`
/// on either kind of failure so the caller can log + skip the row instead
/// of aborting the whole load. The lexer-bug surface area is wide enough
/// that we treat any panic as a "skip this row" signal rather than try to
/// classify upstream's failure modes.
fn try_execute<F>(f: F) -> Result<(), String>
where
    F: FnOnce() -> graphlite_sdk::Result<()>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("error: {e}")),
        Err(p) => {
            let msg = if let Some(s) = p.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = p.downcast_ref::<String>() {
                s.clone()
            } else {
                "<unknown panic>".to_string()
            };
            Err(format!("panic: {msg}"))
        }
    }
}

/// Number of node patterns per batched INSERT statement. The lex/parse/plan
/// overhead is fixed-per-statement in this engine, so batching N patterns
/// gives an ~Nx speedup on bulk loads. Capped at 40 because GraphLite's
/// lexer (`graphlite-0.0.1/src/ast/lexer.rs:326`) enforces a hard
/// 1000-iteration limit per tokenize call to guard against infinite loops;
/// a Comment INSERT pattern is ~18 tokens, so 40 × 18 + INSERT/separators
/// ≈ 760 tokens — comfortably under the cap with headroom for property
/// values that lex to multiple tokens.
const NODE_BATCH: usize = 40;

/// Number of edge patterns per batched MATCH+INSERT statement. We use disjoint
/// alias names per edge (`a0/b0/a1/b1/...`) so each MATCH pair binds the
/// correct endpoints independently. Capped at 15 because each pair uses
/// ~36 tokens (MATCH for two nodes + INSERT for one edge), so 15 × 36 ≈ 540,
/// well under the 1000-iteration lexer cap.
const EDGE_BATCH: usize = 15;

fn load_persons(
    session: &graphlite_sdk::Session,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // We use `session.execute` (auto-commit) instead of opening a single
    // transaction across the whole load. SDK 0.0.1 has a state bug where
    // the SECOND transaction opened on a session rejects valid INSERT
    // statements with `Parse error: UnexpectedToken(Insert)` — first
    // observed when persons committed cleanly but every comment INSERT
    // in the next tx panicked. Auto-commit avoids the transitions
    // entirely. Sled handles the resulting per-row durability cost.
    //
    // Per-row INSERTs through the SDK pipeline (lex+parse+plan+execute) are
    // pathologically slow at LDBC scale (~600K total inserts after persons),
    // so we batch NODE_BATCH patterns into one INSERT. The parser accepts
    // `INSERT (:L {...}), (:L {...}), ...` (comma-separated graph patterns
    // per graphlite-0.0.1 src/ast/parser.rs::insert_statement).
    let mut rdr = ReaderBuilder::new().delimiter(b'|').from_path(path)?;
    let mut n = 0usize;
    let mut skipped = 0usize;
    let mut batch: Vec<String> = Vec::with_capacity(NODE_BATCH);
    let mut flush = |batch: &mut Vec<String>, n: &mut usize, skipped: &mut usize| {
        if batch.is_empty() {
            return;
        }
        let q = format!("INSERT {}", batch.join(", "));
        match try_execute(|| session.execute(&q)) {
            Ok(()) => *n += batch.len(),
            Err(why) => {
                *skipped += batch.len();
                if *skipped <= 500 {
                    eprintln!("    skip person batch (size {}): {why}", batch.len());
                }
            }
        }
        batch.clear();
    };
    for r in rdr.records() {
        let r = r?;
        let id: i64 = r[0].parse()?;
        let first = quote(&ascii_only(&r[1]));
        let last = quote(&ascii_only(&r[2]));
        batch.push(format!(
            "(:Person {{id: {id}, firstName: {first}, lastName: {last}}})"
        ));
        if batch.len() >= NODE_BATCH {
            flush(&mut batch, &mut n, &mut skipped);
        }
    }
    flush(&mut batch, &mut n, &mut skipped);
    eprintln!("    persons: {n} done ({skipped} skipped)");
    Ok(())
}

fn load_messages(
    session: &graphlite_sdk::Session,
    path: &Path,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rdr = ReaderBuilder::new().delimiter(b'|').from_path(path)?;
    // Header has different positions for Comment vs Post; resolve by name.
    let headers = rdr.headers()?.clone();
    let id_idx = headers.iter().position(|h| h == "id").unwrap();
    let cdate_idx = headers.iter().position(|h| h == "creationDate").unwrap();
    let content_idx = headers.iter().position(|h| h == "content");

    // Batched INSERTs for throughput — see comment in load_persons.
    let mut n = 0usize;
    let mut skipped = 0usize;
    let mut batch: Vec<String> = Vec::with_capacity(NODE_BATCH);
    let label_lower = label.to_lowercase();
    let mut flush = |batch: &mut Vec<String>, n: &mut usize, skipped: &mut usize| {
        if batch.is_empty() {
            return;
        }
        let q = format!("INSERT {}", batch.join(", "));
        match try_execute(|| session.execute(&q)) {
            Ok(()) => *n += batch.len(),
            Err(why) => {
                *skipped += batch.len();
                if *skipped <= 500 {
                    eprintln!("    skip {} batch (size {}): {why}", label_lower, batch.len());
                }
            }
        }
        batch.clear();
    };
    for r in rdr.records() {
        let r = r?;
        let id: i64 = r[id_idx].parse()?;
        let cdate: i64 = r[cdate_idx].parse()?;
        let content = match content_idx {
            Some(i) => quote(&ascii_only(&r[i])),
            None => "''".into(),
        };
        batch.push(format!(
            "(:{label} {{id: {id}, creationDate: {cdate}, content: {content}}})"
        ));
        if batch.len() >= NODE_BATCH {
            flush(&mut batch, &mut n, &mut skipped);
        }
    }
    flush(&mut batch, &mut n, &mut skipped);
    eprintln!("    {}: {n} done ({skipped} skipped)", label_lower);
    Ok(())
}

fn load_edges_knows(
    session: &graphlite_sdk::Session,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rdr = ReaderBuilder::new().delimiter(b'|').from_path(path)?;
    // Batched MATCH+INSERT — see comment in load_persons. Each edge in a
    // batch gets disjoint aliases `a0/b0/a1/b1/...` so MATCHes don't bind
    // each other's endpoints.
    let mut n = 0usize;
    let mut skipped = 0usize;
    let mut pairs: Vec<(i64, i64)> = Vec::with_capacity(EDGE_BATCH);
    let mut flush = |pairs: &mut Vec<(i64, i64)>, n: &mut usize, skipped: &mut usize| {
        if pairs.is_empty() {
            return;
        }
        // Two batches: one for (src, dst), one for the reverse direction.
        for direction in 0..2 {
            let mut match_clauses: Vec<String> = Vec::with_capacity(pairs.len() * 2);
            let mut insert_clauses: Vec<String> = Vec::with_capacity(pairs.len());
            for (i, (a_id, b_id)) in pairs.iter().enumerate() {
                let (s, d) = if direction == 0 {
                    (*a_id, *b_id)
                } else {
                    (*b_id, *a_id)
                };
                match_clauses
                    .push(format!("(a{i}:Person {{id: {s}}}), (b{i}:Person {{id: {d}}})"));
                insert_clauses.push(format!("(a{i})-[:knows]->(b{i})"));
            }
            let q = format!(
                "MATCH {} INSERT {}",
                match_clauses.join(", "),
                insert_clauses.join(", ")
            );
            match try_execute(|| session.execute(&q)) {
                Ok(()) => *n += pairs.len(),
                Err(why) => {
                    *skipped += pairs.len();
                    if *skipped <= 500 {
                        eprintln!(
                            "    skip knows batch (size {}, dir {direction}): {why}",
                            pairs.len()
                        );
                    }
                }
            }
        }
        pairs.clear();
    };
    for r in rdr.records() {
        let r = r?;
        let src: i64 = r[0].parse()?;
        let dst: i64 = r[1].parse()?;
        pairs.push((src, dst));
        if pairs.len() >= EDGE_BATCH {
            flush(&mut pairs, &mut n, &mut skipped);
        }
    }
    flush(&mut pairs, &mut n, &mut skipped);
    eprintln!("    knows edges: {n} done ({skipped} skipped, both directions)");
    Ok(())
}

fn load_edges_has_creator(
    session: &graphlite_sdk::Session,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rdr = ReaderBuilder::new().delimiter(b'|').from_path(path)?;
    // Source label varies (Comment vs Post). Detect from filename.
    let label = if path.file_name().unwrap().to_str().unwrap().starts_with("comment_") {
        "Comment"
    } else {
        "Post"
    };
    // Batched MATCH+INSERT — see comment in load_persons.
    let mut n = 0usize;
    let mut skipped = 0usize;
    let mut pairs: Vec<(i64, i64)> = Vec::with_capacity(EDGE_BATCH);
    let label_lower = label.to_lowercase();
    let mut flush = |pairs: &mut Vec<(i64, i64)>, n: &mut usize, skipped: &mut usize| {
        if pairs.is_empty() {
            return;
        }
        let mut match_clauses: Vec<String> = Vec::with_capacity(pairs.len() * 2);
        let mut insert_clauses: Vec<String> = Vec::with_capacity(pairs.len());
        for (i, (s, d)) in pairs.iter().enumerate() {
            match_clauses.push(format!(
                "(a{i}:{label} {{id: {s}}}), (b{i}:Person {{id: {d}}})"
            ));
            insert_clauses.push(format!("(a{i})-[:hasCreator]->(b{i})"));
        }
        let q = format!(
            "MATCH {} INSERT {}",
            match_clauses.join(", "),
            insert_clauses.join(", ")
        );
        match try_execute(|| session.execute(&q)) {
            Ok(()) => *n += pairs.len(),
            Err(why) => {
                *skipped += pairs.len();
                if *skipped <= 500 {
                    eprintln!(
                        "    skip {} hasCreator batch (size {}): {why}",
                        label_lower,
                        pairs.len()
                    );
                }
            }
        }
        pairs.clear();
    };
    for r in rdr.records() {
        let r = r?;
        let src: i64 = r[0].parse()?;
        let dst: i64 = r[1].parse()?;
        pairs.push((src, dst));
        if pairs.len() >= EDGE_BATCH {
            flush(&mut pairs, &mut n, &mut skipped);
        }
    }
    flush(&mut pairs, &mut n, &mut skipped);
    eprintln!(
        "    {} hasCreator edges: {n} done ({skipped} skipped)",
        label_lower
    );
    Ok(())
}
