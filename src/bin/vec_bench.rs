//! Vector-search strategy benchmark — post-filter vs pre-filter vs in-LTJ.
//!
//! Builds a synthetic RDF-shaped graph plus a vector attribute, saves it,
//! builds the HNSW, and then times each strategy on the same queries. The
//! graph, the sidecar, and the LTJ triple index are all constructed
//! before any measurement, so a timing is the strategy and nothing else.
//!
//! Output is CSV on stdout:
//!
//! ```text
//! items,dim,k,mode,selectivity,strategy,source,level,median_ms,recall,nn_pops,nn_expanded,pattern_runs,ltj_visits,candidates,rows
//! ```
//!
//! `recall` is measured against post-filter with the exact cursor, which
//! is the ground truth. It is 1.0 for every exact arm by construction —
//! the differential test pins that — so a value below 1.0 there is a bug,
//! and below 1.0 on an approximate arm is the result being measured.
//!
//! **`nn_pops` per accepted result is the number to read.** With a
//! selective pattern the interleaving arms walk a proximity graph built
//! over the whole corpus, so reaching a candidate that also satisfies the
//! pattern can cost a large fraction of the bottom layer. Post-filter
//! degrades gracefully exactly where those two blow up.
//!
//! Usage:
//!
//! ```text
//! cargo build --release --bin vec_bench
//! ./target/release/vec_bench                       # defaults
//! ./target/release/vec_bench --items 50000 --dim 128 --iters 5
//! ./target/release/vec_bench --ks 1,10,100 --levels 0,1,2
//! ```

use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::time::Instant;

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::runtime::vsearch::{Strategy, VecCfg, VecSource};
use frogql::store::lazy::LazyGraphStore;
use frogql::vector::hnsw::{Hnsw, HnswParams};
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

/// Deterministic xorshift64*: a benchmark that changed its data between
/// runs would smear the comparison it exists to make.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    fn unit(&mut self) -> f32 {
        ((self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0) as f32 * 2.0 - 1.0
    }
}

struct Args {
    items: usize,
    users: usize,
    tags: usize,
    dim: usize,
    likes_per_user: usize,
    ks: Vec<usize>,
    levels: Vec<usize>,
    iters: usize,
    queries: usize,
    seed: u64,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            items: 20_000,
            users: 2_000,
            tags: 50,
            dim: 32,
            likes_per_user: 20,
            ks: vec![1, 10, 100],
            levels: vec![0, 1],
            iters: 5,
            queries: 5,
            seed: 42,
        }
    }
}

fn parse_list(s: &str) -> Vec<usize> {
    s.split(',').filter_map(|p| p.trim().parse().ok()).collect()
}

fn parse_args() -> Args {
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut a = Args::default();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].clone();
        let mut next = || {
            i += 1;
            argv.get(i).cloned().unwrap_or_default()
        };
        match flag.as_str() {
            "--items" => a.items = next().parse().unwrap_or(a.items),
            "--users" => a.users = next().parse().unwrap_or(a.users),
            "--tags" => a.tags = next().parse().unwrap_or(a.tags),
            "--dim" => a.dim = next().parse().unwrap_or(a.dim),
            "--likes" => a.likes_per_user = next().parse().unwrap_or(a.likes_per_user),
            "--ks" => a.ks = parse_list(&next()),
            "--levels" => a.levels = parse_list(&next()),
            "--iters" => a.iters = next().parse().unwrap_or(a.iters),
            "--queries" => a.queries = next().parse().unwrap_or(a.queries),
            "--seed" => a.seed = next().parse().unwrap_or(a.seed),
            "-h" | "--help" => {
                eprintln!(
                    "usage: vec_bench [--items N] [--users N] [--tags N] [--dim N] \
                     [--likes N] [--ks 1,10,100] [--levels 0,1] [--iters N] \
                     [--queries N] [--seed N]"
                );
                std::process::exit(0);
            }
            other => eprintln!("warning: ignoring unknown flag `{other}`"),
        }
        i += 1;
    }
    a
}

/// `(user)-[:likes]->(item)-[:tagged]->(tag)`, plus a `follows` chain
/// among users so a deeper join shape exists.
fn build_graph(a: &Args, rng: &mut Rng) -> MemoryGraphStore {
    let mut nodes = Vec::with_capacity(a.items + a.users + a.tags);
    for i in 0..a.items {
        nodes.push(format!(
            r#"{{"id":"item{i}","labels":["Item"],"props":{{"idx":{i}}}}}"#
        ));
    }
    for u in 0..a.users {
        nodes.push(format!(
            r#"{{"id":"u{u}","labels":["User"],"props":{{"idx":{u}}}}}"#
        ));
    }
    for g in 0..a.tags {
        nodes.push(format!(
            r#"{{"id":"tag{g}","labels":["Tag"],"props":{{"idx":{g}}}}}"#
        ));
    }

    let mut edges = Vec::new();
    let mut e = 0usize;
    for u in 0..a.users {
        for _ in 0..a.likes_per_user {
            let i = rng.below(a.items as u64);
            edges.push(format!(
                r#"{{"id":"e{e}","labels":["likes"],"props":{{}},"endpoints":["u{u}","item{i}"],"directionality":"->"}}"#
            ));
            e += 1;
        }
    }
    for i in 0..a.items {
        let g = rng.below(a.tags as u64);
        edges.push(format!(
            r#"{{"id":"e{e}","labels":["tagged"],"props":{{}},"endpoints":["item{i}","tag{g}"],"directionality":"->"}}"#
        ));
        e += 1;
    }
    for u in 1..a.users {
        edges.push(format!(
            r#"{{"id":"e{e}","labels":["follows"],"props":{{}},"endpoints":["u{}","u{u}"],"directionality":"->"}}"#,
            u - 1
        ));
        e += 1;
    }

    let json = format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    );
    MemoryGraphStore::from_json_str(&json).expect("synthetic graph must parse")
}

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if times.is_empty() {
        return f64::NAN;
    }
    times[times.len() / 2]
}

/// The projected first column of every row, as a set — the unit recall is
/// measured in.
fn key_set(rows: &[Vec<Value>]) -> HashSet<String> {
    rows.iter()
        .filter_map(|r| r.first().map(|v| format!("{v:?}")))
        .collect()
}

fn main() {
    let a = parse_args();
    let mut rng = Rng::new(a.seed);

    let dir = env::temp_dir().join(format!("frogql_vec_bench_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let db = dir.join("bench.gdb");

    eprintln!(
        "building graph: {} items, {} users, {} tags, {} likes/user",
        a.items, a.users, a.tags, a.likes_per_user
    );
    let t = Instant::now();
    build_graph(&a, &mut rng).save(&db).expect("save");
    eprintln!("  saved in {:?}", t.elapsed());

    // Vectors for every Item, then the HNSW. Both offline, so neither
    // lands in a measured query.
    let t = Instant::now();
    let (fp, item_ids) = {
        let store = LazyGraphStore::open(&db).expect("open");
        let fp = fingerprint(store.node_count() as usize, store.edge_count() as usize);
        let mut ids: Vec<u32> = store
            .nodes()
            .into_iter()
            .filter(|id| store.node_name(*id).starts_with("item"))
            .collect();
        ids.sort_unstable();
        (fp, ids)
    };
    let data: Vec<f32> = (0..item_ids.len() * a.dim).map(|_| rng.unit()).collect();
    let set = VectorSet::new("emb".to_string(), a.dim, Metric::L2Sq, fp, item_ids, data);
    eprintln!("  vectors in {:?}, building HNSW…", t.elapsed());
    let t = Instant::now();
    let graph = Hnsw::build(&set, HnswParams::default());
    eprintln!("  HNSW in {:?}", t.elapsed());
    set.with_hnsw(graph)
        .to_sidecar()
        .write_to_path(&Sidecar::path_for(&db, "emb"))
        .expect("write sidecar");

    let store = LazyGraphStore::open(&db).expect("reopen");
    // Warm the LTJ index once and share it: rebuilding it per query would
    // dominate and is not what is being compared.
    let index = Runtime::new(&store).warm_triple_index();
    eprintln!("opened; LTJ index warm. Running…\n");

    println!(
        "items,dim,k,mode,selectivity,strategy,source,level,median_ms,recall,\
         nn_pops,nn_expanded,pattern_runs,ltj_visits,candidates,rows"
    );

    // Two selectivities: `broad` matches every item, `narrow` only the
    // handful one user likes. The gap between them is where the arms
    // diverge, and it is the point of the whole experiment.
    let shapes: Vec<(&str, String)> = vec![
        (
            "broad",
            "MATCH (u:User)-[:likes]->(i:Item), (i)-[:tagged]->(g:Tag)".to_string(),
        ),
        (
            "narrow",
            "MATCH (u:User)-[:likes]->(i:Item), (i)-[:tagged]->(g:Tag) WHERE u.idx = 7".to_string(),
        ),
    ];

    let query_vectors: Vec<String> = (0..a.queries)
        .map(|_| {
            let comps: Vec<String> = (0..a.dim).map(|_| format!("{:.4}", rng.unit())).collect();
            format!("[{}]", comps.join(", "))
        })
        .collect();

    for (sel_name, prefix) in &shapes {
        for &k in &a.ks {
            for qv in &query_vectors {
                let q = format!("{prefix} NEAREST {k} i.emb TO {qv} RETURN i.idx");
                let parsed = match compile_query(&q) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("compile failed: {e}");
                        continue;
                    }
                };

                // Ground truth: post-filter with the exact cursor.
                let rt = Runtime::with_triple_index(&store, index.clone());
                rt.set_vec_cfg(VecCfg {
                    strategy: Strategy::PostFilter,
                    source: VecSource::LocalSort,
                    ..VecCfg::default()
                });
                let truth = match rt.run_query(&parsed, 0) {
                    QueryResult::Projected(rows) => key_set(&rows),
                    _ => HashSet::new(),
                };

                // Every arm the engine can run. `PreFilter` has no
                // per-visit candidate set, so `LocalSort` is not a
                // distinct arm there.
                for (strategy, source) in [
                    (Strategy::PostFilter, VecSource::Hnsw),
                    (Strategy::PostFilter, VecSource::LocalSort),
                    (Strategy::PostFilter, VecSource::GlobalSort),
                    (Strategy::PreFilter, VecSource::Hnsw),
                    (Strategy::PreFilter, VecSource::GlobalSort),
                    (Strategy::InLtj, VecSource::Hnsw),
                    (Strategy::InLtj, VecSource::LocalSort),
                    (Strategy::InLtj, VecSource::GlobalSort),
                ] {
                    // The level axis only exists for in-LTJ.
                    let levels: Vec<usize> = if strategy == Strategy::InLtj {
                        a.levels.clone()
                    } else {
                        vec![0]
                    };
                    for &level in &levels {
                        let rt = Runtime::with_triple_index(&store, index.clone());
                        rt.set_vec_cfg(VecCfg {
                            strategy,
                            source,
                            level,
                            ..VecCfg::default()
                        });

                        // One warmup, then `iters` measured.
                        let _ = rt.run_query(&parsed, 0);
                        let mut times = Vec::with_capacity(a.iters);
                        let mut rows = Vec::new();
                        for _ in 0..a.iters {
                            let t = Instant::now();
                            let out = rt.run_query(&parsed, 0);
                            times.push(t.elapsed().as_secs_f64() * 1000.0);
                            if let QueryResult::Projected(r) = out {
                                rows = r;
                            }
                        }
                        let stats = rt.last_vec_stats();
                        let got = key_set(&rows);
                        let recall = if truth.is_empty() {
                            1.0
                        } else {
                            got.intersection(&truth).count() as f64 / truth.len() as f64
                        };

                        println!(
                            "{},{},{k},distinct,{sel_name},{},{},{level},{:.3},{recall:.3},{},{},{},{},{},{}",
                            a.items,
                            a.dim,
                            strategy.name(),
                            source.name(),
                            median(times),
                            stats.nn_pops,
                            stats.nn_expanded,
                            stats.pattern_runs,
                            stats.ltj_visits,
                            stats.candidates_hashed,
                            rows.len(),
                        );

                        // A strategy that silently degraded is not the
                        // strategy the row claims to measure.
                        let expected = strategy.name();
                        if !stats.arm.starts_with(expected) {
                            eprintln!(
                                "warning: asked for {expected}, ran {} ({}) — {q}",
                                stats.arm,
                                stats.fallback_reason.unwrap_or_default()
                            );
                        }
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    let _: PathBuf = db;
}
