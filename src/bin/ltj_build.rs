//! Build the LTJ index once and write it beside the database.
//!
//! ```text
//! ltj_build <db.gdb> [--compact] [--force]
//!
//!   --compact   build the CLTJ succinct representation instead of the
//!               six sorted arrays (same as FROGQL_LTJ_COMPACT=1). The
//!               sidecar records which one it holds, and a session
//!               configured for the other rebuilds rather than loading it.
//!   --force     rewrite even when a valid sidecar is already there
//! ```
//!
//! The index is a pure function of the graph, and building it is
//! `O(E log E)` landing entirely at open: 670 ms at LDBC SF0.1, and 252
//! seconds — measured — on a 617 M-edge RDF dump, once per session, every
//! session. This computes it once.
//!
//! After that, `frogql <db.gdb>` finds `<db>.ltj` and loads it. The loaded
//! index is the same structure the builder produces, filled from a file
//! rather than computed, so a query cannot tell them apart; what changes
//! is that opening reads instead of sorting.
//!
//! Rerun it after any change to the database. The sidecar carries a
//! `(node count, edge count)` fingerprint and is ignored when the graph no
//! longer matches, so a stale file costs a rebuild rather than a wrong
//! answer — but it is a coarse signature, and a delete plus an
//! equal-sized insert would slip past it. Deleting the sidecar always
//! forces a rebuild.

use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use frogql::model::graph_access::GraphAccess;
use frogql::runtime::ltj::persist;
use frogql::runtime::ltj::triple_index::TripleIndex;
use frogql::store::lazy::LazyGraphStore;

fn usage() -> ! {
    eprintln!(
        "usage: ltj_build <db.gdb> [--compact] [--force]\n\
         \n\
         options:\n  \
           --compact   build the succinct CLTJ representation (smaller, slower to query)\n  \
           --force     rewrite even when a valid sidecar already exists"
    );
    process::exit(2)
}

fn main() {
    let mut db: Option<PathBuf> = None;
    let mut compact = false;
    let mut force = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--compact" => compact = true,
            "--force" => force = true,
            "-h" | "--help" => usage(),
            other if other.starts_with('-') => {
                eprintln!("error: unknown flag `{other}`");
                usage()
            }
            other => {
                if db.is_some() {
                    eprintln!("error: unexpected argument `{other}`");
                    usage();
                }
                db = Some(PathBuf::from(other));
            }
        }
    }
    let db = db.unwrap_or_else(|| usage());

    // The representation is chosen by the same env var the engine reads,
    // so the flag cannot select one thing and the build another.
    if compact {
        std::env::set_var("FROGQL_LTJ_COMPACT", "1");
    }
    let compact = TripleIndex::compact_selected();

    let t_open = Instant::now();
    let store = match LazyGraphStore::open(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", db.display());
            process::exit(1);
        }
    };
    let (nodes, edges) = match store.index_sidecar_key() {
        Some((_, n, e)) => (n, e),
        None => {
            eprintln!("error: {} has no on-disk identity", db.display());
            process::exit(1);
        }
    };
    eprintln!(
        "opened {} ({nodes} nodes, {edges} edges) in {:.1}s",
        db.display(),
        t_open.elapsed().as_secs_f64()
    );

    let path = persist::path_for(&db);
    if !force {
        if let Ok(existing) = persist::read_for(&db, nodes, edges, compact) {
            let n = existing.len();
            println!(
                "{} is already current ({n} triples, {} representation); \
                 pass --force to rewrite",
                path.display(),
                if compact { "compact" } else { "array" }
            );
            return;
        }
    }

    let t_build = Instant::now();
    let index = TripleIndex::from_graph(&store);
    eprintln!(
        "built the {} index over {} triples in {:.1}s",
        if compact { "compact" } else { "array" },
        index.len(),
        t_build.elapsed().as_secs_f64()
    );

    let t_write = Instant::now();
    if let Err(e) = persist::write_to_path(&index, &path, nodes, edges) {
        eprintln!("error: cannot write {}: {e}", path.display());
        process::exit(1);
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {} — {} representation, {} triples, {:.2} GiB, written in {:.1}s",
        path.display(),
        if compact { "compact" } else { "array" },
        index.len(),
        bytes as f64 / (1024.0 * 1024.0 * 1024.0),
        t_write.elapsed().as_secs_f64()
    );

    // A sidecar that cannot be read back is worse than none: the engine
    // would fall through to a rebuild and the file would be dead weight
    // nobody notices. Prove the round trip before claiming success.
    match persist::read_for(&db, nodes, edges, compact) {
        Ok(back) if back.len() == index.len() => {
            eprintln!("verified: reads back as {} triples", back.len())
        }
        Ok(back) => {
            eprintln!(
                "error: wrote {} triples but read back {}",
                index.len(),
                back.len()
            );
            process::exit(1);
        }
        Err(why) => {
            eprintln!("error: the sidecar just written does not load: {why}");
            process::exit(1);
        }
    }
    let _: &Path = &path;
}
