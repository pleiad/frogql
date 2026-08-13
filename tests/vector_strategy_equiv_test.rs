//! Differential test: the three evaluation strategies must agree.
//!
//! This is what makes the benchmark legitimate. If post-filter,
//! pre-filter, and in-LTJ can disagree on the answer, comparing their
//! latency compares three different queries.
//!
//! Agreement is asserted under the **exact** cursor (`use_index = false`).
//! With HNSW the arms walk the proximity graph differently and their
//! recall genuinely differs — that difference is a result to measure, not
//! a bug to assert away. What is asserted for the approximate arms is
//! that they never invent a row the pattern does not produce.
//!
//! The same discipline as `compact_ltj_test.rs`: one optimisation, one
//! kill switch, one test pinning "optimised ≡ baseline".

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::runtime::vsearch::{Strategy, VecCfg};
use frogql::store::lazy::LazyGraphStore;
use frogql::vector::hnsw::{Hnsw, HnswParams};
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

/// Deterministic xorshift64*, so a failure is reproducible.
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
        self.next_u64() % n
    }
    fn unit(&mut self) -> f32 {
        ((self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0) as f32 * 2.0 - 1.0
    }
}

const USERS: usize = 12;
const ITEMS: usize = 60;
const TAGS: usize = 6;
const DIM: usize = 4;

/// A small RDF-shaped graph with enough structure that the join has
/// several variables and therefore several legal VEO levels:
///
/// ```text
/// (user)-[:likes]->(item)-[:tagged]->(tag)
/// (user)-[:follows]->(user)
/// ```
fn fixture_json(rng: &mut Rng) -> String {
    let mut nodes = Vec::new();
    for i in 0..ITEMS {
        nodes.push(format!(
            r#"{{"id":"item{i}","labels":["Item"],"props":{{"idx":{i}}}}}"#
        ));
    }
    for u in 0..USERS {
        nodes.push(format!(
            r#"{{"id":"u{u}","labels":["User"],"props":{{"idx":{u}}}}}"#
        ));
    }
    for g in 0..TAGS {
        nodes.push(format!(
            r#"{{"id":"tag{g}","labels":["Tag"],"props":{{"idx":{g}}}}}"#
        ));
    }

    let mut edges = Vec::new();
    let mut e = 0usize;
    let push = |edges: &mut Vec<String>, label: &str, a: String, b: String, e: &mut usize| {
        edges.push(format!(
            r#"{{"id":"e{}","labels":["{label}"],"props":{{}},"endpoints":["{a}","{b}"],"directionality":"->"}}"#,
            *e
        ));
        *e += 1;
    };
    // Each user likes 5 random items; each item carries one tag.
    for u in 0..USERS {
        let mut seen = HashSet::new();
        for _ in 0..5 {
            let i = rng.below(ITEMS as u64) as usize;
            if seen.insert(i) {
                push(
                    &mut edges,
                    "likes",
                    format!("u{u}"),
                    format!("item{i}"),
                    &mut e,
                );
            }
        }
    }
    for i in 0..ITEMS {
        let g = rng.below(TAGS as u64) as usize;
        push(
            &mut edges,
            "tagged",
            format!("item{i}"),
            format!("tag{g}"),
            &mut e,
        );
    }
    for u in 0..USERS {
        let v = rng.below(USERS as u64) as usize;
        if v != u {
            push(
                &mut edges,
                "follows",
                format!("u{u}"),
                format!("u{v}"),
                &mut e,
            );
        }
    }

    format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    )
}

fn build_db(name: &str, seed: u64) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_vec_equiv_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");

    let mut rng = Rng::new(seed);
    MemoryGraphStore::from_json_str(&fixture_json(&mut rng))
        .unwrap()
        .save(&db)
        .unwrap();

    let (fp, item_ids) = {
        let store = LazyGraphStore::open(&db).unwrap();
        use frogql::model::graph_access::GraphAccess;
        let fp = fingerprint(store.node_count() as usize, store.edge_count() as usize);
        let mut ids: Vec<u32> = store
            .nodes()
            .into_iter()
            .filter(|id| store.node_name(*id).starts_with("item"))
            .collect();
        ids.sort_unstable();
        (fp, ids)
    };

    let data: Vec<f32> = (0..item_ids.len() * DIM).map(|_| rng.unit()).collect();
    let set = VectorSet::new("emb".to_string(), DIM, Metric::L2Sq, fp, item_ids, data);
    let h = Hnsw::build(&set, HnswParams::default());
    set.with_hnsw(h)
        .to_sidecar()
        .write_to_path(&Sidecar::path_for(&db, "emb"))
        .unwrap();
    db
}

/// Run `q` and return the projected rows.
fn run(db: &Path, q: &str, cfg: VecCfg) -> Vec<Vec<Value>> {
    let store = LazyGraphStore::open(db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_vec_cfg(cfg);
    let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    }
}

fn exact(strategy: Strategy, level: usize) -> VecCfg {
    VecCfg {
        strategy,
        use_index: false,
        level,
        ..VecCfg::default()
    }
}

/// Query vectors written out as literals so every arm gets byte-identical
/// input.
fn query_vectors(seed: u64, n: usize) -> Vec<String> {
    let mut rng = Rng::new(seed);
    (0..n)
        .map(|_| {
            let comps: Vec<String> = (0..DIM).map(|_| format!("{:.4}", rng.unit())).collect();
            format!("[{}]", comps.join(", "))
        })
        .collect()
}

fn queries(vec_literal: &str, k: usize, rows_mode: bool) -> Vec<String> {
    let m = if rows_mode { "ROWS " } else { "" };
    vec![
        // One join variable above the search variable.
        format!(
            "MATCH (u:User)-[:likes]->(i:Item) NEAREST {k} {m}i.emb TO {vec_literal} RETURN i.idx, u.idx"
        ),
        // Three variables: the search variable can sit at several levels.
        format!(
            "MATCH (u:User)-[:likes]->(i:Item), (i)-[:tagged]->(g:Tag) \
             NEAREST {k} {m}i.emb TO {vec_literal} RETURN i.idx, u.idx, g.idx"
        ),
        // Selective: one user only.
        format!(
            "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 3 \
             NEAREST {k} {m}i.emb TO {vec_literal} RETURN i.idx"
        ),
        // Four variables, two hops of users.
        format!(
            "MATCH (a:User)-[:follows]->(b:User), (b)-[:likes]->(i:Item) \
             NEAREST {k} {m}i.emb TO {vec_literal} RETURN i.idx, a.idx, b.idx"
        ),
    ]
}

/// Rows as a multiset, so an arm is free to produce them in any order
/// within one distance — only the *content* of the answer is pinned here.
fn bag(rows: &[Vec<Value>]) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    v.sort();
    v
}

#[test]
fn the_three_strategies_agree_exactly_under_the_exact_cursor() {
    let db = build_db("exact", 7);
    for vec_literal in query_vectors(11, 4) {
        for k in [1usize, 3, 8] {
            for rows_mode in [false, true] {
                for q in queries(&vec_literal, k, rows_mode) {
                    let oracle = run(&db, &q, exact(Strategy::PostFilter, 0));
                    for (strategy, level) in [
                        (Strategy::PreFilter, 0),
                        (Strategy::InLtj, 0),
                        (Strategy::InLtj, 1),
                        (Strategy::InLtj, 2),
                        (Strategy::InLtj, 3),
                    ] {
                        let got = run(&db, &q, exact(strategy, level));
                        assert_eq!(
                            bag(&got),
                            bag(&oracle),
                            "\nstrategy {strategy:?} level {level}\nquery: {q}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_veo_level_does_not_change_the_answer() {
    // Isolating the level axis: only in-LTJ, only the level varying.
    let db = build_db("levels", 21);
    let vec_literal = &query_vectors(3, 1)[0];
    for k in [1usize, 5] {
        for q in queries(vec_literal, k, false) {
            let base = run(&db, &q, exact(Strategy::InLtj, 0));
            for level in 1..5 {
                assert_eq!(
                    bag(&run(&db, &q, exact(Strategy::InLtj, level))),
                    bag(&base),
                    "\nlevel {level}\nquery: {q}"
                );
            }
        }
    }
}

#[test]
fn every_arm_returns_at_most_k_bindings_in_distinct_var_mode() {
    let db = build_db("k_bound", 5);
    let vec_literal = &query_vectors(2, 1)[0];
    for k in [1usize, 2, 4] {
        let q = format!(
            "MATCH (u:User)-[:likes]->(i:Item) NEAREST {k} i.emb TO {vec_literal} RETURN i.idx"
        );
        for (strategy, use_index) in [
            (Strategy::PostFilter, false),
            (Strategy::PostFilter, true),
            (Strategy::PreFilter, false),
            (Strategy::PreFilter, true),
            (Strategy::InLtj, false),
            (Strategy::InLtj, true),
        ] {
            let got = run(
                &db,
                &q,
                VecCfg {
                    strategy,
                    use_index,
                    ..VecCfg::default()
                },
            );
            let distinct: HashSet<String> = got.iter().map(|r| format!("{:?}", r[0])).collect();
            assert!(
                distinct.len() <= k,
                "{strategy:?} index={use_index} returned {} distinct bindings for k={k}",
                distinct.len()
            );
        }
    }
}

#[test]
fn every_arm_returns_at_most_k_rows_in_rows_mode() {
    let db = build_db("k_bound_rows", 15);
    let vec_literal = &query_vectors(4, 1)[0];
    for k in [1usize, 3, 7] {
        let q = format!(
            "MATCH (u:User)-[:likes]->(i:Item) NEAREST {k} ROWS i.emb TO {vec_literal} RETURN i.idx"
        );
        for (strategy, use_index) in [
            (Strategy::PostFilter, false),
            (Strategy::PostFilter, true),
            (Strategy::PreFilter, false),
            (Strategy::PreFilter, true),
            (Strategy::InLtj, false),
            (Strategy::InLtj, true),
        ] {
            let got = run(
                &db,
                &q,
                VecCfg {
                    strategy,
                    use_index,
                    ..VecCfg::default()
                },
            );
            assert!(
                got.len() <= k,
                "{strategy:?} index={use_index} returned {} rows for k={k}",
                got.len()
            );
        }
    }
}

#[test]
fn an_approximate_arm_never_invents_a_row() {
    // Recall may differ under HNSW, but every row an approximate arm
    // returns must be one the pattern actually produces. A row that is
    // not in the unfiltered match is a bug, not a recall difference.
    let db = build_db("no_invention", 9);
    for vec_literal in query_vectors(6, 3) {
        for q in queries(&vec_literal, 5, false) {
            let unfiltered_q = q.split(" NEAREST ").next().unwrap().to_string()
                + &q[q.find(" RETURN ").unwrap()..];
            let universe: HashSet<String> = bag(&run(&db, &unfiltered_q, VecCfg::default()))
                .into_iter()
                .collect();

            for strategy in [Strategy::PostFilter, Strategy::PreFilter, Strategy::InLtj] {
                let got = run(
                    &db,
                    &q,
                    VecCfg {
                        strategy,
                        use_index: true,
                        ..VecCfg::default()
                    },
                );
                for row in bag(&got) {
                    assert!(
                        universe.contains(&row),
                        "{strategy:?} produced a row the pattern does not: {row}\nquery: {q}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_in_ltj_arm_actually_ran_rather_than_falling_back() {
    // A silent fallback would make the equivalence assertions above pass
    // for the wrong reason.
    let db = build_db("no_silent_fallback", 31);
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    for level in 0..3 {
        rt.set_vec_cfg(exact(Strategy::InLtj, level));
        let query = frogql::compile_query(
            "MATCH (u:User)-[:likes]->(i:Item), (i)-[:tagged]->(g:Tag) \
             NEAREST 3 i.emb TO [0.1, 0.2, 0.3, 0.4] RETURN i.idx",
        )
        .unwrap();
        let _ = rt.run_query(&query, 0);
        let stats = rt.last_vec_stats();
        assert_eq!(stats.arm, "inltj+brute", "level {level}");
        assert!(
            stats.ltj_visits > 0,
            "level {level}: the level was never reached"
        );
        assert_eq!(stats.pattern_runs, 1, "one join, not one per candidate");
    }
}

#[test]
fn the_threshold_prunes_the_neighbour_walk() {
    // The point of the strategy: a small k must not walk the whole
    // attribute. Compared against k = every item, where it must.
    let db = build_db("pruning", 41);
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let mut pops = Vec::new();
    for k in [1usize, ITEMS] {
        rt.set_vec_cfg(exact(Strategy::InLtj, 0));
        let query = frogql::compile_query(&format!(
            "MATCH (u:User)-[:likes]->(i:Item) NEAREST {k} i.emb TO [0.0, 0.0, 0.0, 0.0] \
             RETURN i.idx"
        ))
        .unwrap();
        let _ = rt.run_query(&query, 0);
        pops.push(rt.last_vec_stats().nn_pops);
    }
    assert!(
        pops[0] < pops[1],
        "k=1 popped {} neighbours, k={ITEMS} popped {} — the threshold is not cutting",
        pops[0],
        pops[1]
    );
}

#[test]
fn the_shared_prefix_is_reused_across_visits() {
    // Off level 0 the search reaches the level once per binding above it,
    // and each visit re-walks the stream from the nearest. If the prefix
    // cache were not shared, every visit would re-drive the cursor.
    let db = build_db("prefix_reuse", 51);
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_vec_cfg(exact(Strategy::InLtj, 1));
    // Two triples, so `i` is non-lonely and level 1 is legal. With a
    // single triple every variable is lonely, `max_level` is 0, and the
    // request would be clamped back to a single visit.
    let query = frogql::compile_query(
        "MATCH (u:User)-[:likes]->(i:Item), (i)-[:tagged]->(g:Tag) \
         NEAREST 3 i.emb TO [0.0, 0.0, 0.0, 0.0] RETURN i.idx",
    )
    .unwrap();
    let _ = rt.run_query(&query, 0);
    let stats = rt.last_vec_stats();
    assert!(stats.ltj_visits > 1, "expected several visits to the level");
    assert!(
        stats.prefix_replays > stats.prefix_extends,
        "visits={} replays={} extends={}: the prefix is not being reused",
        stats.ltj_visits,
        stats.prefix_replays,
        stats.prefix_extends
    );
}
