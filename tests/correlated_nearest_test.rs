//! Correlated `NEAREST`: the query vector comes out of the pattern.
//!
//! `NEAREST k y.emb TO VECTOR(x, 'emb')` asks a different question from
//! the uncorrelated clause the four strategies answer. It is a similarity
//! *join*: one ranking per distinct binding of `x`, not one ranking for
//! the query. Before this arm existed the clause parsed and typechecked,
//! then silently returned zero rows — `resolve_spec` evaluates the query
//! vector against an empty assignment, so a pattern variable there is an
//! unbound reference, and an unresolvable query vector is (correctly, for
//! the uncorrelated case) an empty answer.
//!
//! The fixture is arithmetic rather than random so every expected answer
//! can be read off by hand: candidates sit at `2i` along the diagonal of
//! a 4-space, anchors sit at chosen points, and the L2-squared distance
//! between two diagonal points `a` and `b` is `4(a-b)^2`.

use std::path::{Path, PathBuf};

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::runtime::vsearch::{Strategy, VecCfg, VecSource};
use frogql::store::lazy::LazyGraphStore;
use frogql::vector::hnsw::{Hnsw, HnswParams};
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

const DIM: usize = 4;
/// Diagonal coordinate of each candidate, by index: `c0` at 0, `c1` at 2,
/// and so on.
const CANDIDATES: usize = 8;
/// Diagonal coordinate of each anchor.
const ANCHORS: [f32; 3] = [0.0, 10.0, -5.0];

/// ```text
/// (hub)-[:P69]->(cN)          candidates, all reachable from one hub
/// (aN)-[:P69]->(anchorN)      each anchor image hangs off its own node
/// ```
///
/// Both arms of the join are anchored on a constant so the pattern is
/// deterministic and small; what varies per row is the anchor image,
/// which is exactly the correlation under test.
/// The graph plus `orphans` isolated nodes: images that carry a
/// vector but appear in no triple. They cannot satisfy any pattern, so
/// they can never be an answer — but a ranking source that walks the
/// whole attribute still walks past them.
fn fixture_json_with_orphans(orphans: usize) -> String {
    // Distinct labels rather than a `WHERE`: a residual predicate is
    // dropped by the LTJ decomposition, so the in-LTJ arms decline a
    // query that carries one and this fixture would silently measure the
    // fallback instead of the arm under test.
    let mut nodes = vec![r#"{"id":"hub","labels":["Img","Hub"],"props":{"idx":-1}}"#.to_string()];
    for i in 0..CANDIDATES {
        nodes.push(format!(
            r#"{{"id":"c{i}","labels":["Img"],"props":{{"idx":{i}}}}}"#
        ));
    }
    for (i, _) in ANCHORS.iter().enumerate() {
        nodes.push(format!(
            r#"{{"id":"anchor{i}","labels":["Img"],"props":{{"idx":{}}}}}"#,
            100 + i
        ));
        nodes.push(format!(
            r#"{{"id":"holder{i}","labels":["Img","Holder"],"props":{{"idx":{}}}}}"#,
            200 + i
        ));
    }

    let mut edges = Vec::new();
    let mut e = 0usize;
    for i in 0..CANDIDATES {
        edges.push(format!(
            r#"{{"id":"e{e}","labels":["P69"],"props":{{}},"endpoints":["hub","c{i}"],"directionality":"->"}}"#
        ));
        e += 1;
    }
    for i in 0..ANCHORS.len() {
        edges.push(format!(
            r#"{{"id":"e{e}","labels":["P69"],"props":{{}},"endpoints":["holder{i}","anchor{i}"],"directionality":"->"}}"#
        ));
        e += 1;
    }

    for i in 0..orphans {
        nodes.push(format!(
            r#"{{"id":"orphan{i}","labels":["Img"],"props":{{"idx":{}}}}}"#,
            1000 + i
        ));
    }

    format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    )
}

fn build_db(name: &str) -> PathBuf {
    build_db_with_orphans(name, 0)
}

/// Build a database whose vector attribute covers the graph's images
/// *and* `orphans` images that carry a descriptor but appear in no
/// triple. That is the shape the IMGpedia dumps have — only ~10% of the
/// images with a HOG vector occur in the triple file — and importing the
/// graph alone silently shrinks the search corpus to that 10%.
fn build_db_with_orphans(name: &str, orphans: usize) -> PathBuf {
    // Interleaved with the anchors and candidates: a corpus-wide ranking
    // really has to walk past them.
    build_db_with_orphans_at(name, orphans, -6.0)
}

/// The same, with the orphan block placed at `base` along the diagonal.
/// A base past every candidate puts the whole block *after* the last
/// candidate in every anchor's ranking, which is what separates a walk
/// bounded by the candidate set from one bounded by the corpus.
fn build_db_with_orphans_at(name: &str, orphans: usize, base: f32) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_corr_nearest_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");

    MemoryGraphStore::from_json_str(&fixture_json_with_orphans(orphans))
        .unwrap()
        .save(&db)
        .unwrap();

    let store = LazyGraphStore::open(&db).unwrap();
    let fp = fingerprint(store.node_count() as usize, store.edge_count() as usize);
    // Ids must ascend, and the coordinate must follow the node the id
    // belongs to — the sidecar is positional, so pairing them by name is
    // the only safe way to build it.
    let mut rows: Vec<(u32, f32)> = Vec::new();
    for id in store.nodes() {
        let name = store.node_name(id).to_string();
        if let Some(i) = name.strip_prefix("c").and_then(|s| s.parse::<usize>().ok()) {
            rows.push((id, 2.0 * i as f32));
        } else if let Some(i) = name
            .strip_prefix("anchor")
            .and_then(|s| s.parse::<usize>().ok())
        {
            rows.push((id, ANCHORS[i]));
        } else if let Some(i) = name
            .strip_prefix("orphan")
            .and_then(|s| s.parse::<usize>().ok())
        {
            rows.push((id, base + i as f32 * 0.1));
        }
    }
    rows.sort_by_key(|(id, _)| *id);
    let ids: Vec<u32> = rows.iter().map(|(id, _)| *id).collect();
    let data: Vec<f32> = rows.iter().flat_map(|(_, x)| [*x; DIM]).collect();

    let set = VectorSet::new("emb".to_string(), DIM, Metric::L2Sq, fp, ids, data);
    let h = Hnsw::build(&set, HnswParams::default());
    set.with_hnsw(h)
        .to_sidecar()
        .write_to_path(&Sidecar::path_for(&db, "emb"))
        .unwrap();
    db
}

/// Run `q` and return `(anchor idx, candidate idx, distance)` triples,
/// deduplicated and sorted, so the assertion is on the answer rather than
/// on how many pattern rows carried it.
fn run(db: &Path, q: &str, source: VecSource) -> Vec<(i64, i64, f32)> {
    run_with(db, q, Strategy::PostFilter, source)
}

fn run_with(db: &Path, q: &str, strategy: Strategy, source: VecSource) -> Vec<(i64, i64, f32)> {
    let store = LazyGraphStore::open(db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_vec_cfg(VecCfg {
        strategy,
        source,
        ..VecCfg::default()
    });
    let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    let mut out: Vec<(i64, i64, f32)> = rows
        .iter()
        .map(|r| {
            use frogql::model::value::Value;
            let int = |v: &Value| match v {
                Value::Int(n) => *n,
                other => panic!("expected an int, got {other:?}"),
            };
            let f = match &r[2] {
                Value::Float(f) => *f as f32,
                other => panic!("expected a float distance, got {other:?}"),
            };
            (int(&r[0]), int(&r[1]), f)
        })
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap());
    out.dedup();
    out
}

const QUERY: &str = "MATCH (h:Holder)-[:P69]->(v00:Img), (hub:Hub)-[:P69]->(v11:Img) \
     NEAREST 2 v11.emb TO VECTOR(v00, 'emb') AS d \
     RETURN v00.idx, v11.idx, d";

/// The whole point: one ranking per anchor, each against its own vector.
///
/// Anchors sit at 0, 10 and -5; candidate `ci` sits at `2i`; the metric
/// is L2-squared over four equal components, so a coordinate gap `g`
/// costs `4g^2`.
#[test]
fn correlated_nearest_ranks_per_anchor() {
    let db = build_db("per_anchor");
    let got = run(&db, QUERY, VecSource::LocalSort);
    let want = vec![
        // anchor0 at 0  -> c0 (gap 0, d 0), c1 (gap 2, d 16)
        (100, 0, 0.0),
        (100, 1, 16.0),
        // anchor1 at 10 -> c5 (gap 0, d 0), c4 (gap 2, d 16)
        (101, 4, 16.0),
        (101, 5, 0.0),
        // anchor2 at -5 -> c0 (gap 5, d 100), c1 (gap 7, d 196)
        (102, 0, 100.0),
        (102, 1, 196.0),
    ];
    let mut want = want;
    want.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(got, want);
}

/// The three ranking sources differ in where the nearest-first stream
/// comes from, not in what the answer is. On an exact source that is an
/// invariant; HNSW's recall is a measurement, but on a corpus this small
/// its walk is exhaustive, so it must agree too.
#[test]
fn correlated_nearest_agrees_across_sources() {
    let db = build_db("sources");
    let local = run(&db, QUERY, VecSource::LocalSort);
    let global = run(&db, QUERY, VecSource::GlobalSort);
    let hnsw = run(&db, QUERY, VecSource::Hnsw);
    assert_eq!(local, global, "the two exact sources must agree");
    assert_eq!(local, hnsw, "HNSW is exhaustive on a corpus this small");
}

/// A `k` wider than the candidate set keeps every candidate, still ranked
/// per anchor — the arm must not confuse "no more neighbours" with "no
/// more anchors".
#[test]
fn correlated_nearest_k_beyond_candidates() {
    let db = build_db("wide_k");
    let q = QUERY.replace("NEAREST 2 ", "NEAREST 99 ");
    let got = run(&db, &q, VecSource::LocalSort);
    assert_eq!(
        got.len(),
        ANCHORS.len() * CANDIDATES,
        "every anchor should keep every candidate"
    );
    for (anchor, _, _) in &got {
        assert!((100..100 + ANCHORS.len() as i64).contains(anchor));
    }
}

/// An uncorrelated clause on the same database must still take the
/// ordinary arm. The correlation test is "does the query vector name a
/// pattern variable", so a constant node id is not one.
#[test]
fn constant_query_vector_stays_uncorrelated() {
    let db = build_db("uncorrelated");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let anchor0 = store
        .nodes()
        .into_iter()
        .find(|id| store.node_name(*id) == "anchor0")
        .expect("anchor0");
    let q = format!(
        "MATCH (hub:Img)-[:P69]->(v11:Img) WHERE hub.idx = -1 \
         NEAREST 2 v11.emb TO VECTOR({anchor0}, 'emb') AS d \
         RETURN v11.idx, d"
    );
    let query = frogql::compile_query(&q).unwrap();
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    assert_eq!(rows.len(), 2);
    assert!(
        rt.last_vec_stats().arm.starts_with("post")
            || rt.last_vec_stats().arm.starts_with("interleave")
            || rt.last_vec_stats().arm.starts_with("memo")
            || rt.last_vec_stats().arm.starts_with("pre"),
        "a constant query vector must not take the correlated arm, got {}",
        rt.last_vec_stats().arm
    );
}

/// One pattern evaluation for all anchors, and one ranking per anchor.
/// Partitioning after a single run is the whole reason the arm is not
/// "re-run the pattern pinned per anchor".
#[test]
fn correlated_nearest_runs_the_pattern_once() {
    let db = build_db("stats");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_vec_cfg(VecCfg {
        source: VecSource::LocalSort,
        ..VecCfg::default()
    });
    let query = frogql::compile_query(QUERY).unwrap();
    let _ = rt.run_query(&query, 0);
    let stats = rt.last_vec_stats();
    assert_eq!(stats.pattern_runs, 1, "the pattern must run exactly once");
    assert_eq!(
        stats.anchor_groups,
        ANCHORS.len() as u64,
        "one partition per distinct anchor binding"
    );
    assert!(
        stats.arm.starts_with("correlated"),
        "expected the correlated arm, got {}",
        stats.arm
    );
}

// ---------------------------------------------------------------------------
// Corpus size
// ---------------------------------------------------------------------------

/// Images that carry a vector but appear in no triple must not change the
/// answer — and must change the cost.
///
/// On the IMGpedia dumps only ~10% of the images with a HOG descriptor
/// occur in the triple file, so importing the graph alone yields a sidecar
/// covering a tenth of the corpus. The answers are the same either way: an
/// image with no triple satisfies no pattern, so it could never have been
/// returned. What differs is how much a ranking source that walks the
/// whole attribute has to walk past — which is exactly the quantity a
/// similarity benchmark is measuring. `import_ttl --nodes-from` exists to
/// put those images back.
///
/// The asymmetry between the sources is the point, and it is why "just
/// use localsort" is not an answer: the two exact sources disagree about
/// whether the corpus is even visible.
#[test]
fn orphan_vectors_change_the_cost_but_not_the_answer() {
    const ORPHANS: usize = 200;
    let small = build_db_with_orphans("corpus_small", 0);
    let full = build_db_with_orphans("corpus_full", ORPHANS);

    for source in [VecSource::LocalSort, VecSource::GlobalSort] {
        assert_eq!(
            run(&small, QUERY, source),
            run(&full, QUERY, source),
            "an image with no triple can never be an answer ({source:?})"
        );
    }

    // `localsort` ranks only the pattern's candidates, so the corpus is
    // invisible to it; `globalsort` sorts the whole attribute, so it is
    // not. Counted through the engine's own stats rather than a clock.
    let pops = |db: &Path, source: VecSource| -> u64 {
        let store = LazyGraphStore::open(db).unwrap();
        let rt = Runtime::new(&store);
        rt.set_vec_cfg(VecCfg {
            source,
            ..VecCfg::default()
        });
        let q = frogql::compile_query(QUERY).unwrap();
        let _ = rt.run_query(&q, 0);
        rt.last_vec_stats().nn_pops
    };

    assert_eq!(
        pops(&small, VecSource::LocalSort),
        pops(&full, VecSource::LocalSort),
        "localsort never looks outside the pattern's candidates"
    );

    let small_global = pops(&small, VecSource::GlobalSort);
    let full_global = pops(&full, VecSource::GlobalSort);
    assert!(
        full_global > small_global * 4,
        "globalsort walks the whole attribute, so {ORPHANS} orphans must \
         cost it materially more than {small_global} pops; got {full_global}"
    );
}

// ---------------------------------------------------------------------------
// The in-LTJ form of a correlated clause
// ---------------------------------------------------------------------------

/// `interleave` over a correlated clause must answer exactly what the
/// partitioning arm answers.
///
/// This is the arm the study needs and the one that did not exist: a
/// correlated clause has one ranking per anchor, and the in-LTJ hook was
/// built around a single ranking fixed before the search. It works now
/// because the anchor is forced *above* the search variable in the
/// variable elimination order (`VeoOverride::pin_at_after`), so by the
/// time a visit reaches the search level the anchor is bound and its
/// vector is readable — and the three things that were per-query become
/// per-anchor: the vector, the top-`k` threshold, and the corpus stream.
///
/// Equality with the partitioning arm is the whole claim. Anything else
/// would mean the two answer different questions and their latencies
/// cannot be compared, which is the only reason to have both.
#[test]
fn correlated_interleave_agrees_with_partitioning() {
    let db = build_db("corr_interleave");
    for source in [VecSource::LocalSort, VecSource::GlobalSort, VecSource::Hnsw] {
        let partitioned = run_with(&db, QUERY, Strategy::PostFilter, source);
        let in_ltj = run_with(&db, QUERY, Strategy::Interleave, source);
        assert_eq!(
            in_ltj, partitioned,
            "correlated interleave must agree with partitioning ({source:?})"
        );
        assert!(!partitioned.is_empty(), "the fixture must produce rows");
    }
}

/// And it must actually be the in-LTJ arm, not a quiet fallback to
/// partitioning. `ltj_visits` is the tell: the partitioning arm never
/// enters the join's search level, so it reports zero.
#[test]
fn correlated_interleave_really_runs_in_the_join() {
    let db = build_db("corr_interleave_arm");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let q = frogql::compile_query(QUERY).unwrap();

    rt.set_vec_cfg(VecCfg {
        strategy: Strategy::Interleave,
        source: VecSource::LocalSort,
        ..VecCfg::default()
    });
    let _ = rt.run_query(&q, 0);
    let s = rt.last_vec_stats();
    assert_eq!(s.arm, "interleave+localsort", "got {}", s.arm);
    assert!(
        s.ltj_visits > 0,
        "the search must reach the level in the join"
    );
    assert_eq!(
        s.anchor_groups,
        ANCHORS.len() as u64,
        "one ranking per distinct anchor"
    );

    rt.set_vec_cfg(VecCfg {
        strategy: Strategy::PostFilter,
        source: VecSource::LocalSort,
        ..VecCfg::default()
    });
    let _ = rt.run_query(&q, 0);
    let s = rt.last_vec_stats();
    assert_eq!(s.arm, "correlated+localsort", "got {}", s.arm);
    assert_eq!(s.ltj_visits, 0, "partitioning never enters the join level");
}

/// `pre` has no correlated form, so it must partition — and say so,
/// rather than report an arm that did not run.
#[test]
fn pre_falls_back_with_a_reason() {
    let db = build_db("corr_fallback");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let q = frogql::compile_query(QUERY).unwrap();

    rt.set_vec_cfg(VecCfg {
        strategy: Strategy::PreFilter,
        source: VecSource::LocalSort,
        ..VecCfg::default()
    });
    let _ = rt.run_query(&q, 0);
    let s = rt.last_vec_stats();
    assert!(
        s.arm.starts_with("correlated"),
        "pre must partition, got {}",
        s.arm
    );
    assert!(
        s.fallback_reason.is_some(),
        "pre must record why it did not run"
    );
}

/// `k` counts per anchor, not per query: widening it must widen every
/// anchor's answer, in both arms alike.
#[test]
fn correlated_interleave_counts_k_per_anchor() {
    let db = build_db("corr_k");
    let wide = QUERY.replace("NEAREST 2 ", "NEAREST 99 ");
    for strategy in [Strategy::PostFilter, Strategy::Interleave] {
        let got = run_with(&db, &wide, strategy, VecSource::LocalSort);
        assert_eq!(
            got.len(),
            ANCHORS.len() * CANDIDATES,
            "{strategy:?}: every anchor should keep every candidate"
        );
    }
}

// ---------------------------------------------------------------------------
// `memo` over a correlated clause
// ---------------------------------------------------------------------------

/// The third arm must answer exactly what the other two answer.
///
/// `memo` hoists the ranking walk out of the visit; correlated, "out of
/// the visit" means "one walk per anchor" rather than one walk for the
/// query, since there is no global ranking when the query vector varies.
/// If the three disagree they are answering different questions and
/// comparing their latencies means nothing, which is the only reason to
/// keep three.
#[test]
fn correlated_memo_agrees_with_partitioning() {
    let db = build_db("corr_memo");
    for source in [VecSource::LocalSort, VecSource::GlobalSort, VecSource::Hnsw] {
        let partitioned = run_with(&db, QUERY, Strategy::PostFilter, source);
        let interleaved = run_with(&db, QUERY, Strategy::Interleave, source);
        let memo = run_with(&db, QUERY, Strategy::Memo, source);
        assert_eq!(
            memo, partitioned,
            "correlated memo must agree with partitioning ({source:?})"
        );
        assert_eq!(
            memo, interleaved,
            "the two in-LTJ arms must agree with each other ({source:?})"
        );
        assert!(!partitioned.is_empty(), "the fixture must produce rows");
    }
}

/// A corpus of images with no triples must not change the answer here
/// either — it is the shape that made the walk cuts worth adding.
#[test]
fn correlated_memo_agrees_with_a_wider_corpus() {
    let db = build_db_with_orphans("corr_memo_orphans", 200);
    for source in [VecSource::LocalSort, VecSource::GlobalSort] {
        assert_eq!(
            run_with(&db, QUERY, Strategy::Memo, source),
            run_with(&db, QUERY, Strategy::PostFilter, source),
            "correlated memo must agree with partitioning ({source:?})"
        );
    }
}

/// And it must really be the in-LTJ arm. `ltj_visits` says the join was
/// entered, `suffix_resumes` says phase 2 completed prefixes phase 1 had
/// collected — the pair is what distinguishes `memo` from every other arm.
#[test]
fn correlated_memo_really_runs_in_the_join() {
    let db = build_db("corr_memo_arm");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let q = frogql::compile_query(QUERY).unwrap();

    rt.set_vec_cfg(VecCfg {
        strategy: Strategy::Memo,
        source: VecSource::LocalSort,
        ..VecCfg::default()
    });
    let _ = rt.run_query(&q, 0);
    let s = rt.last_vec_stats();
    assert_eq!(s.arm, "memo+localsort", "got {}", s.arm);
    assert!(
        s.fallback_reason.is_none(),
        "no fallback expected, got {:?}",
        s.fallback_reason
    );
    assert!(s.ltj_visits > 0, "phase 1 must reach the level in the join");
    assert!(s.suffix_resumes > 0, "phase 2 must resume stored prefixes");
    assert_eq!(
        s.anchor_groups,
        ANCHORS.len() as u64,
        "one ranking per distinct anchor"
    );
}

/// `k` counts per anchor here too: widening it widens every anchor's
/// answer, and the three arms stay equal while it does.
#[test]
fn correlated_memo_counts_k_per_anchor() {
    let db = build_db("corr_memo_k");
    let wide = QUERY.replace("NEAREST 2 ", "NEAREST 99 ");
    for strategy in [Strategy::PostFilter, Strategy::Interleave, Strategy::Memo] {
        let got = run_with(&db, &wide, strategy, VecSource::LocalSort);
        assert_eq!(
            got.len(),
            ANCHORS.len() * CANDIDATES,
            "{strategy:?}: every anchor should keep every candidate"
        );
    }
}

/// The two walk cuts, measured in `nn_pops` rather than on a clock.
///
/// `FROGQL_VEC_LEVEL` is 0 here, so every visit is its own anchor and
/// phase 2 runs one walk per anchor — eleven of them, since the search
/// variable's anchor is any image the pattern's second triple can reach,
/// not only the three that survive the whole join.
///
/// **Cut 1 — `k` held.** Every accepted result is at least as near as the
/// entry the walk is standing on, and `TopK` refuses a tie once full, so
/// nothing later can enter. Worth one pop per walk: without it the walk
/// pops one further entry to discover it is past the threshold.
///
/// **Cut 2 — every candidate seen.** The table is empty, so no remaining
/// entry of the stream can be in it, whatever the threshold says. This is
/// the one that matters: it caps the walk at the depth of the *last*
/// candidate instead of at the end of the corpus, which is what the
/// orphan-heavy corpus below makes visible.
///
/// Both are A/B'd against `memo_cuts: false`, and the answer must not
/// move — a cut that changes the answer is a bug, not an optimization.
fn pops_and_rows(db: &Path, q: &str, k: usize, cuts: bool) -> (u64, Vec<(i64, i64, f32)>) {
    let store = LazyGraphStore::open(db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_vec_cfg(VecCfg {
        strategy: Strategy::Memo,
        source: VecSource::GlobalSort,
        memo_cuts: cuts,
        ..VecCfg::default()
    });
    let text = q.replace("NEAREST 2 ", &format!("NEAREST {k} "));
    let query = frogql::compile_query(&text).unwrap();
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    let s = rt.last_vec_stats();
    assert_eq!(s.arm, "memo+globalsort", "got {}", s.arm);
    let mut out: Vec<(i64, i64, f32)> = rows
        .iter()
        .map(|r| {
            use frogql::model::value::Value;
            let int = |v: &Value| match v {
                Value::Int(n) => *n,
                other => panic!("expected an int, got {other:?}"),
            };
            let f = match &r[2] {
                Value::Float(f) => *f as f32,
                other => panic!("expected a float distance, got {other:?}"),
            };
            (int(&r[0]), int(&r[1]), f)
        })
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap());
    out.dedup();
    (s.nn_pops, out)
}

/// Cut 1, on its own: `k = 2` fills the threshold quickly and the corpus
/// is the eleven pattern images, so the distance cut would end each walk
/// one entry later than the fullness cut does.
#[test]
fn correlated_memo_stops_as_soon_as_k_are_held() {
    let db = build_db("corr_memo_cut_k");
    let (cut, rows_cut) = pops_and_rows(&db, QUERY, 2, true);
    let (uncut, rows_uncut) = pops_and_rows(&db, QUERY, 2, false);
    assert_eq!(rows_cut, rows_uncut, "the cut must not move the answer");
    assert!(
        cut < uncut,
        "stopping at the threshold rather than past it must cost fewer \
         pops: got {cut} against {uncut}"
    );
}

/// Cut 2, on its own: `k = 99` is wider than any anchor's candidate set,
/// so the threshold never fills and neither distance cut can fire. The
/// only thing that can end a walk is having drained the table.
///
/// What the cut is worth depends entirely on **where the last candidate
/// sits in the ranking**, and the two corpora here are the two ends of
/// that. With the orphans interleaved among the candidates the last
/// candidate is near the end of the corpus anyway and the cut trims a few
/// percent. With the orphans placed past every candidate — the shape a
/// real corpus has, where the pattern's images are a sliver of the
/// attribute — the walk ends at the eleventh entry instead of the two
/// hundred and eleventh.
#[test]
fn correlated_memo_stops_once_every_candidate_is_seen() {
    let near = build_db_with_orphans("corr_memo_cut_seen", 200);
    let (cut, rows_cut) = pops_and_rows(&near, QUERY, 99, true);
    let (uncut, rows_uncut) = pops_and_rows(&near, QUERY, 99, false);
    assert_eq!(rows_cut, rows_uncut, "the cut must not move the answer");
    assert!(
        cut < uncut,
        "interleaved orphans: got {cut} pops against an uncut {uncut}"
    );

    let far = build_db_with_orphans_at("corr_memo_cut_far", 200, 1000.0);
    let (cut, rows_cut) = pops_and_rows(&far, QUERY, 99, true);
    let (uncut, rows_uncut) = pops_and_rows(&far, QUERY, 99, false);
    assert_eq!(rows_cut, rows_uncut, "the cut must not move the answer");
    assert!(
        cut * 5 < uncut,
        "orphans past every candidate: the walk must stop at the last \
         candidate, not at the end of the corpus; got {cut} pops against \
         an uncut {uncut}"
    );
}

/// The level is the knob the study turns, so equality has to hold at
/// every one of them, not only at the default.
///
/// It is also where the two in-LTJ arms are supposed to diverge in cost:
/// at level 0 there is one visit per anchor and `memo` has nothing to
/// hoist, while deeper down an anchor has many visits and `interleave`
/// re-walks its ranking once per visit. The counters are reported by
/// `FROGQL_DEBUG_VEC`; what is pinned here is that the divergence is in
/// cost alone.
#[test]
fn correlated_memo_agrees_at_every_level() {
    let db = build_db("corr_memo_levels");
    let oracle = run_with(&db, QUERY, Strategy::PostFilter, VecSource::LocalSort);
    for level in 0..4 {
        for source in [VecSource::LocalSort, VecSource::GlobalSort] {
            for strategy in [Strategy::Interleave, Strategy::Memo] {
                let store = LazyGraphStore::open(&db).unwrap();
                let rt = Runtime::new(&store);
                rt.set_vec_cfg(VecCfg {
                    strategy,
                    source,
                    level,
                    ..VecCfg::default()
                });
                let q = frogql::compile_query(QUERY).unwrap();
                let rows = match rt.run_query(&q, 0) {
                    QueryResult::Projected(rows) => rows,
                    other => panic!("expected a projection, got {other:?}"),
                };
                let mut got: Vec<(i64, i64, f32)> = rows
                    .iter()
                    .map(|r| {
                        use frogql::model::value::Value;
                        let int = |v: &Value| match v {
                            Value::Int(n) => *n,
                            other => panic!("expected an int, got {other:?}"),
                        };
                        let f = match &r[2] {
                            Value::Float(f) => *f as f32,
                            other => panic!("expected a float distance, got {other:?}"),
                        };
                        (int(&r[0]), int(&r[1]), f)
                    })
                    .collect();
                got.sort_by(|a, b| a.partial_cmp(b).unwrap());
                got.dedup();
                assert_eq!(
                    got, oracle,
                    "{strategy:?} + {source:?} at level {level} must match the oracle"
                );
            }
        }
    }
}
