use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Path, PathValue, Value};
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{
    Aggregator, GeneralSetKind, MatchStatement, NullsOrder, Query, ReturnItem, SetQuantifier,
    SortDir, SortKey, SortSpec,
};
use crate::typing::descriptor_type::DescriptorType;
use crate::typing::label_type::LabelType;
use crate::typing::property_type::PropertyType;
use crate::typing::simple_type::SimpleType;

use super::assignment::Assignment;
use super::cmp_values;
use super::ltj::pattern_extract;
use super::ltj::triple_index::TripleIndex;
use super::path_select::apply_path_prefix;
use super::path_select::path_satisfies_mode;
use super::result::{ExprResult, IntermediateResult, QueryResult, ResultRow};
use crate::syntax::path_prefix::{PathMode, PathPrefix, UnboundedSupport};

/// Finite evaluation policy for unbounded repetition inside `Selected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnboundedPolicy {
    /// No finite-making prefix; rejected by the typechecker.
    Forbidden,
    /// WALK + SHORTEST: length-ordered k-shortest search.
    Shortest { count: usize, groups: bool },
    /// TRAIL / SIMPLE / ACYCLIC: finite enumeration with mode pruning.
    Mode(PathMode),
}

fn unbounded_policy_for(prefix: Option<PathPrefix>) -> UnboundedPolicy {
    match prefix.and_then(|p| p.unbounded_support()) {
        Some(UnboundedSupport::Shortest { count, groups }) => {
            UnboundedPolicy::Shortest { count, groups }
        }
        Some(UnboundedSupport::Mode(m)) => UnboundedPolicy::Mode(m),
        None => UnboundedPolicy::Forbidden,
    }
}

/// Number of edges in a path = its length for shortest-path ranking.
fn path_edge_len(path: &Path) -> usize {
    path.0.iter().filter(|pv| pv.is_edge()).count()
}

/// Min-heap entry for the k-shortest repetition search. `BinaryHeap` is a
/// max-heap, so `Ord` is reversed to pop the *shortest* path first; the
/// monotone `seq` tie-breaks equal lengths deterministically.
struct ShortestEntry {
    len: usize,
    seq: usize,
    reps: usize,
    row: ResultRow,
}

impl PartialEq for ShortestEntry {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.seq == other.seq
    }
}
impl Eq for ShortestEntry {}
impl PartialOrd for ShortestEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ShortestEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: a smaller length compares as "greater" so the max-heap
        // yields it first; tie-break on the smaller seq.
        other
            .len
            .cmp(&self.len)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

/// Apply value predicates pushed down by the optimizer to raw graph properties.
/// Missing key → predicate is null → reject.
fn check_value_preds(preds: &[(String, BinOp, Value)], props: &Props) -> bool {
    preds
        .iter()
        .all(|(attr, op, expected)| match props.get(attr) {
            Some(actual) => cmp_values(actual, *op, expected),
            None => false,
        })
}

/// Smaller cap wins; both inputs honor the runtime's `0 = unbounded`
/// convention. Callers must short-circuit `Query.limit == Some(0)`
/// upstream — that's a real "return zero rows" request and would
/// otherwise be mistranslated to "unbounded" here.
fn combine_limits(query_limit: Option<u32>, runtime_limit: usize) -> usize {
    match (query_limit, runtime_limit) {
        (None, n) => n,
        (Some(q), 0) => q as usize,
        (Some(q), n) => (q as usize).min(n),
    }
}

/// Runtime engine for evaluating GQL path patterns on a graph.
/// Generic over `GraphAccess` — works with both in-memory MemoryGraphStore and file-backed GraphStore.
///
/// Optimizations:
/// - Label-indexed scanning: uses label index when descriptor has a simple label
/// - Adjacency-driven concat: uses adjacency lists when right side is an edge/node pattern
pub struct Runtime<'g, G: GraphAccess> {
    pub graph: &'g G,
    /// Shared LTJ TripleIndex. Built lazily on the first query that needs
    /// it; held as an Arc so callers (REPL, Python `Connection`, benches)
    /// can pre-build at open time and pass the same instance into every
    /// fresh Runtime they spawn — without rebuilding the ~700ms (SF0.1) /
    /// multi-second (SF1) six-ordering sort each time. Wrapped in a
    /// RefCell so internal lazy initialization stays an immutable-self
    /// API for callers.
    triple_index: RefCell<Option<Arc<TripleIndex>>>,
    /// Memoization for `EXISTS` / `NOT EXISTS` predicates. Keyed by
    /// the body's `Box<Query>` heap address, which stays stable while
    /// the compiled AST is alive. Across queries the address is *not*
    /// stable: once a query AST is dropped, its allocation can be
    /// recycled by a subsequent parse with a different body. The
    /// runtime clears the cache at the top of every public entry
    /// point (`run`, `run_with_limit`, `run_query`) to keep the
    /// memoization scoped to one top-level execution. The cached
    /// value is one of:
    ///   - `Uncorrelated(bool)` — the body shares no variable with
    ///     the outer scope; the bool records whether any row exists.
    ///   - `Correlated { keys, set }` — the body shares variables
    ///     with the outer scope; `set` holds the value-tuples on
    ///     those `keys` for every body row, so a per-outer-row probe
    ///     is one O(1) hash lookup. The body runs once per Runtime.
    exists_cache: RefCell<HashMap<usize, ExistsCache>>,
    /// Set only while evaluating `PathPattern::Selected`.
    unbounded_policy: Cell<UnboundedPolicy>,
}

/// Cached evaluation result for an existential predicate.
enum ExistsCache {
    Uncorrelated(bool),
    Correlated {
        /// Correlation variables, sorted by name so probe-key order is
        /// deterministic across rows.
        keys: Vec<String>,
        /// Hash of `(value at keys[0], value at keys[1], ...)` tuples
        /// for every row of the body's evaluation.
        set: HashSet<Vec<PathValue>>,
    },
}

impl<'g, G: GraphAccess> Runtime<'g, G> {
    pub fn new(graph: &'g G) -> Self {
        Self {
            graph,
            triple_index: RefCell::new(None),
            exists_cache: RefCell::new(HashMap::new()),
            unbounded_policy: Cell::new(UnboundedPolicy::Forbidden),
        }
    }

    /// Construct a Runtime that already knows about a pre-built LTJ
    /// TripleIndex — typically built once when the database is opened and
    /// shared (via Arc) across every Runtime spawned for that connection.
    /// Cheap: the Arc clone is one atomic increment.
    pub fn with_triple_index(graph: &'g G, idx: Arc<TripleIndex>) -> Self {
        Self {
            graph,
            triple_index: RefCell::new(Some(idx)),
            exists_cache: RefCell::new(HashMap::new()),
            unbounded_policy: Cell::new(UnboundedPolicy::Forbidden),
        }
    }

    /// Build the LTJ TripleIndex now (if not already cached) and return the
    /// shared Arc. Use it in long-lived sessions (REPL, Python Connection,
    /// benchmarks) at open time to amortize the build cost — every Runtime
    /// constructed afterwards via `with_triple_index` is instant.
    pub fn warm_triple_index(&self) -> Arc<TripleIndex> {
        self.triple_index().clone()
    }

    /// Drop the cached LTJ TripleIndex and EXISTS memo. The next query
    /// that needs the index will rebuild it from the (possibly mutated)
    /// graph. Callers invoke this after a successful DML so subsequent
    /// reads see the post-mutation state.
    pub fn invalidate_caches(&self) {
        *self.triple_index.borrow_mut() = None;
        self.exists_cache.borrow_mut().clear();
    }

    /// Lazily build (or return) the cached LTJ TripleIndex. Idempotent —
    /// called from every `run_join` / `run_concat_pattern` site that needs
    /// the index, but the build only runs once per `Runtime` instance
    /// (and never if `with_triple_index` already provided one).
    fn triple_index(&self) -> Arc<TripleIndex> {
        if self.triple_index.borrow().is_none() {
            let idx = Arc::new(TripleIndex::from_graph(self.graph));
            *self.triple_index.borrow_mut() = Some(idx);
        }
        self.triple_index
            .borrow()
            .clone()
            .expect("triple index just built")
    }

    pub fn run(&self, pattern: &PathPattern) -> IntermediateResult {
        self.exists_cache.borrow_mut().clear();
        // A raw pattern carries no selected-prefix context, so unbounded repetition
        // (if any) has no license here.
        self.unbounded_policy.set(UnboundedPolicy::Forbidden);
        self.run_path_pattern(pattern, 0)
    }

    /// Run with a result limit (0 = unlimited). Stops early once limit is reached.
    pub fn run_with_limit(&self, pattern: &PathPattern, limit: usize) -> IntermediateResult {
        self.exists_cache.borrow_mut().clear();
        self.unbounded_policy.set(UnboundedPolicy::Forbidden);
        self.run_path_pattern(pattern, limit)
    }

    /// Run a full Query (MATCH ... WHERE ... RETURN [LIMIT N]).
    ///
    /// `limit` semantics differ between the two paths: with no aggregates
    /// it caps input rows (early termination); with aggregates it caps
    /// output rows after grouping (truncating input would corrupt counts).
    /// `Query.limit == Some(0)` short-circuits to empty per ISO §LIMIT;
    /// otherwise the in-query and caller caps combine via `combine_limits`.
    pub fn run_query(&self, query: &Query, limit: usize) -> QueryResult {
        // The exists_cache is keyed by body heap address, which is only
        // unique while the AST is alive. A long-lived Runtime (REPL,
        // benches) reuses freed addresses across queries, so a stale
        // entry from a prior body would silently satisfy a new EXISTS
        // probe. Scope memoization to one top-level execution.
        self.exists_cache.borrow_mut().clear();
        // ISO §LIMIT: `Some(0)` is "return zero rows", distinct from
        // the runtime's `0 = unbounded`. Honor it before any pattern work.
        if query.limit == Some(0) {
            return match &query.returns {
                None => QueryResult::Raw(IntermediateResult::empty()),
                Some(_) => QueryResult::Projected(Vec::new()),
            };
        }
        let limit = combine_limits(query.limit, limit);
        let has_order = query.order_by.is_some();
        let has_column_sort_key = query
            .order_by
            .as_ref()
            .is_some_and(|specs| specs.iter().any(|s| matches!(s.key, SortKey::Column(_))));

        // BTree-LTJ "real": drive the primary sort variable from the
        // btree in key order, calling the LTJ once per `(value, ids)`
        // pair with the variable pre-pinned. Replaces the
        // `run_match_chain → sort` flow with `~k` LTJ runs and a
        // cohort-aware early-exit at LIMIT k. Auto-activates whenever
        // the precondition holds; falls back closed (returns None) on
        // any precondition miss, so adding it to the auto path costs
        // nothing for queries that can't use it. Force off with
        // `GQLITE_ORDERBY_FORCE=pdqsort` or `topk`.
        let force = std::env::var("GQLITE_ORDERBY_FORCE").ok();
        let force_off = matches!(
            force.as_deref(),
            Some("pdqsort") | Some("topk") | Some("driftsort")
        );
        let real_rows = if has_order && !force_off {
            self.try_btree_ltj_real(query, limit)
        } else {
            None
        };

        let return_items = match &query.returns {
            None => {
                if let Some(rows) = real_rows {
                    let mut ir = IntermediateResult::new(Vec::new());
                    ir.rows = rows;
                    if limit > 0 && ir.rows.len() > limit {
                        ir.rows.truncate(limit);
                    }
                    return QueryResult::Raw(ir);
                }
                let input_limit = if has_order { 0 } else { limit };
                let mut ir = self.run_match_chain(query, input_limit);
                if let Some(specs) = &query.order_by {
                    self.route_pre_sort(&mut ir.rows, specs, &query.matches, limit);
                    if limit > 0 && ir.rows.len() > limit {
                        ir.rows.truncate(limit);
                    }
                }
                return QueryResult::Raw(ir);
            }
            Some(items) => items,
        };

        let has_aggs = return_items.iter().any(|i| i.is_aggregate());
        let needs_full_input = has_aggs || query.distinct || has_order;
        let input_limit = if needs_full_input { 0 } else { limit };

        let (mut ir, used_real) = match real_rows {
            Some(rows) => (IntermediateResult::new(rows), true),
            None => (self.run_match_chain(query, input_limit), false),
        };

        let pre_projection_sort = !has_aggs && !has_column_sort_key && !used_real;
        if pre_projection_sort {
            if let Some(specs) = &query.order_by {
                // DISTINCT post-projection means sort sees more rows than
                // it needs to keep — top-k can't safely cut here.
                let sort_limit = if query.distinct { 0 } else { limit };
                self.route_pre_sort(&mut ir.rows, specs, &query.matches, sort_limit);
            }
        }

        let mut projected = if has_aggs {
            let p = self.run_aggregated(return_items, query.group_by.as_deref(), &ir.rows);
            if query.distinct {
                dedup_preserving_order(p)
            } else {
                p
            }
        } else {
            self.run_row_by_row(return_items, &ir.rows, query.distinct)
        };

        if has_order && !pre_projection_sort && !used_real {
            if let Some(specs) = &query.order_by {
                sort_projected_rows(&mut projected, specs, limit);
            }
        }

        if limit > 0 && projected.len() > limit {
            projected.truncate(limit);
        }

        QueryResult::Projected(projected)
    }

    /// Route the pre-projection sort to one of three implementations:
    /// pdqsort (full O(n log n)), top-k heap (O(n log k) when `limit < n / 2`),
    /// or a btree-driven bucket sort that walks ids already in attribute
    /// order. The btree path applies only when the spec is a single
    /// `var.attr` AttrLookup AND the store has a btree index on
    /// `(label, attr)` for some label declared on `var`. When the btree
    /// short-circuits, it returns `true`; otherwise we fall back to
    /// `sort_rows` (which itself decides between top-k and pdqsort).
    ///
    /// The env var `GQLITE_ORDERBY_FORCE` controls routing for benches:
    /// `pdqsort` / `topk` skip the btree branch; `btree-ltj` enables it
    /// (and falls through to pdqsort if precondition not met).
    fn route_pre_sort(
        &self,
        rows: &mut Vec<ResultRow>,
        specs: &[SortSpec],
        matches: &[MatchStatement],
        limit: usize,
    ) {
        let force = std::env::var("GQLITE_ORDERBY_FORCE").ok();
        let try_btree = !matches!(
            force.as_deref(),
            Some("pdqsort") | Some("topk") | Some("driftsort")
        );
        if try_btree && self.try_btree_sort(rows, specs, matches, limit) {
            return;
        }
        self.sort_rows(rows, specs, limit);
    }

    /// BTree-LTJ "real": drive the sort variable through the btree in key
    /// order, calling `try_ltj_with_pin` once per `(value, [ids])` entry
    /// so the LTJ itself never enumerates the variable's domain. Stops
    /// at `LIMIT k` without ever materializing rows beyond k.
    ///
    /// Precondition (the lessons from bitácora §13 baked in):
    /// - Single-spec ORDER BY of shape `var.attr`.
    /// - The pattern has at least one edge — otherwise the LTJ doesn't
    ///   fire and the per-iteration cost degrades to O(n) post-scan.
    /// - The store has a btree on `(label, attr)` for some label on `var`.
    /// - No aggregates, no GROUP BY, no DISTINCT, no NULLS FIRST.
    /// - The btree covers every node carrying the label (no nulls).
    ///
    /// Falls back to `None` on any precondition miss; caller routes
    /// through the standard `run_match_chain → route_pre_sort` flow.
    fn try_btree_ltj_real(&self, query: &Query, limit: usize) -> Option<Vec<ResultRow>> {
        if query.distinct {
            return None;
        }
        if let Some(items) = &query.returns {
            if items.iter().any(|i| i.is_aggregate()) {
                return None;
            }
        }
        if query.group_by.is_some() {
            return None;
        }
        let specs = query.order_by.as_ref()?;
        if specs.is_empty() {
            return None;
        }
        // Primary spec drives the btree walk. Secondaries (if any) are
        // resolved by the in-memory sort over the surviving cohort.
        // All specs must be `SortKey::Expr` so the in-memory sort can
        // evaluate them against the binding-table assignment; any
        // `SortKey::Column` would have to land on the post-projection
        // path, which is incompatible with cohort-driven top-k.
        if specs.iter().any(|s| matches!(s.key, SortKey::Column(_))) {
            return None;
        }
        let primary = &specs[0];
        let SortKey::Expr(Expr::AttrLookup { var, attr }) = &primary.key else {
            return None;
        };
        if matches!(primary.nulls.unwrap_or(NullsOrder::Last), NullsOrder::First) {
            return None;
        }

        // Edge precondition: the LTJ only fires for patterns that
        // decompose into ≥1 triple, and a triple needs an edge.
        if !query.matches.iter().any(|m| pattern_has_edge(m.pattern())) {
            return None;
        }

        // Collapse the match chain into the same shape `run_match_chain`
        // would feed to LTJ. This path supports only single-MATCH chains
        // (no OPTIONAL / multi-MATCH); running k small LTJs already
        // assumes a single decomposable pattern.
        if query.has_any_optional() || query.matches.len() > 1 {
            return None;
        }

        let asc = matches!(primary.dir, SortDir::Asc);
        let labels = labels_for_var(&query.matches, var);
        // Build the ordered-id stream: one btree's `Vec<Id>` for the
        // single-label / And case, or a k-way merge of per-label
        // cursors for `Or`-of-leaves descriptors. Both produce ids in
        // primary-attr sort order.
        let mut cursors: Vec<BtreeMergeCursor> = if !labels.is_empty() {
            // Single-label / required-labels path: pick the first label
            // with a btree on `attr`. Walk that one cursor in order.
            // The btree-merge with a single cursor degenerates to the
            // original linear walk.
            let (label, ordered_ids) = labels.iter().find_map(|l| {
                self.graph
                    .lookup_node_ordered(l, attr, asc)
                    .map(|ids| (l.clone(), ids))
            })?;
            let label_total = self.graph.nodes_with_label(&label)?.len();
            if ordered_ids.len() != label_total {
                return None;
            }
            vec![BtreeMergeCursor::new(self.graph, label, ordered_ids, attr)]
        } else {
            // Multi-label `Or`-of-leaves path: every candidate must
            // have a btree on `attr`. Per-label coverage may be
            // partial (the auto-built btree skips nodes whose `attr`
            // is null); the merge still walks every value-bearing id
            // in primary-attr order, which is sufficient under the
            // already-enforced `NULLS LAST` precondition.
            let or_labels = or_candidate_labels_for_var(&query.matches, var)?;
            if std::env::var("GQLITE_DEBUG_INDEXES").is_ok() {
                eprintln!(
                    "btree-ltj-real: or-merge over labels {:?} on attr {:?}",
                    or_labels, attr
                );
            }
            let mut cs = Vec::with_capacity(or_labels.len());
            for l in or_labels {
                let ids = match self.graph.lookup_node_ordered(&l, attr, asc) {
                    Some(v) => v,
                    None => {
                        if std::env::var("GQLITE_DEBUG_INDEXES").is_ok() {
                            eprintln!(
                                "btree-ltj-real:   {} no btree on (label, attr) — fallback",
                                l
                            );
                        }
                        return None;
                    }
                };
                let label_total = match self.graph.nodes_with_label(&l) {
                    Some(v) => v.len(),
                    None => return None,
                };
                if std::env::var("GQLITE_DEBUG_INDEXES").is_ok() {
                    eprintln!(
                        "btree-ltj-real:   {} ids={} label_total={}",
                        l,
                        ids.len(),
                        label_total
                    );
                }
                // Allow `ids.len() < label_total`: the auto-built btree
                // omits nodes whose `attr` is null (the index can't key
                // on `Null`). Walking only the indexed ids is correct
                // for `NULLS LAST` (the precondition we already
                // enforce up-front for the primary spec) — null-valued
                // nodes sort to the end and so can't enter the top-k
                // unless the requested limit exceeds the non-null
                // count, in which case the merge produces all the
                // indexed rows anyway and the caller's downstream sort
                // / truncation handles the remainder. The common case
                // (LDBC IC2 with `creationDate < ...`) excludes nulls
                // up front via 3VL.
                if ids.len() > label_total {
                    return None;
                }
                cs.push(BtreeMergeCursor::new(self.graph, l, ids, attr));
            }
            cs
        };

        let pattern = query.collapsed_pattern();
        let index = self.triple_index();
        let cap = if limit == 0 { usize::MAX } else { limit };
        let single_spec = specs.len() == 1;
        let multi_label = cursors.len() > 1;
        let mut out: Vec<ResultRow> = Vec::with_capacity(cap);
        // De-dup ids that appear in two different label cursors at
        // once (a node carrying both `Comment` and `Post`, say).
        // Single-cursor walks can't repeat an id, so skip the cost.
        let mut visited: HashSet<Id> = HashSet::new();

        // Cohort-aware early exit (multi-spec case): once we have ≥ k
        // rows AND the next id's primary value differs from the
        // previously processed id's, every remaining id has a
        // strictly worse primary key and can't enter the top-k. Stop,
        // then sort the buffer by all specs (primary already in
        // sorted order; secondaries resolve in-memory) and truncate
        // to k. The single-spec case gets the original early-exit at
        // `out.len() >= cap` — no secondary tie-break to consider.
        let mut last_primary_value: Option<Value> = None;
        while let Some(idx) = pick_best_cursor(&cursors, asc) {
            let id = cursors[idx]
                .current_id()
                .expect("pick_best_cursor returns only live cursors");
            let cur_primary = cursors[idx].head_value.clone();

            if multi_label && !visited.insert(id) {
                cursors[idx].advance(self.graph, attr);
                continue;
            }

            if !single_spec
                && out.len() >= cap
                && last_primary_value.is_some()
                && cur_primary != last_primary_value
            {
                break;
            }

            let ir = self.pinned_run(&pattern, &index, var, id)?;
            for row in ir.rows {
                out.push(row);
                if single_spec && out.len() >= cap {
                    return Some(out);
                }
            }
            last_primary_value = cur_primary;
            cursors[idx].advance(self.graph, attr);
        }

        if !single_spec && specs.len() > 1 && !out.is_empty() {
            self.sort_rows(&mut out, specs, cap);
        }
        if cap < out.len() {
            out.truncate(cap);
        }
        Some(out)
    }

    /// Per-pin execution helper for the btree-LTJ-real walk. Calls
    /// `try_ltj_with_pin` on flat-chain leaves directly; for `Union`
    /// (introduced by the unroll pass) recurses into each arm with
    /// the same pin and concatenates the row sets, mirroring the
    /// runtime's own `Union` evaluator. `Filter` is also unwrapped:
    /// run the pinned inner, then post-filter the rows with `expr`.
    /// Returns `None` only when one of the leaf chains is itself
    /// non-decomposable — the caller propagates that to the standard
    /// `run_match_chain` fallback.
    fn pinned_run(
        &self,
        pattern: &PathPattern,
        index: &TripleIndex,
        var: &str,
        id: u32,
    ) -> Option<IntermediateResult> {
        match pattern {
            PathPattern::Union(a, b) => {
                let ir_a = self.pinned_run(a, index, var, id)?;
                let ir_b = self.pinned_run(b, index, var, id)?;
                let mut rows = ir_a.rows;
                rows.extend(ir_b.rows);
                Some(IntermediateResult::new(rows))
            }
            PathPattern::Filter(inner, expr) => {
                let ir = self.pinned_run(inner, index, var, id)?;
                let filtered: Vec<ResultRow> = ir
                    .rows
                    .into_iter()
                    .filter(|r| self.run_expr(&r.assignment, expr).get_bool())
                    .collect();
                Some(IntermediateResult::new(filtered))
            }
            _ => pattern_extract::try_ltj_with_pin(self.graph, pattern, index, 0, var, id),
        }
    }

    /// Btree-driven pre-projection sort. Precondition: `specs` is a
    /// single `SortKey::Expr(AttrLookup { var, attr })` AND the variable
    /// carries some label `L` for which the store has a btree index on
    /// `(L, attr)`. Returns true on success (rows replaced with the
    /// btree-ordered output, capped at `limit` if non-zero); false when
    /// the precondition does not hold.
    ///
    /// The btree gives ids in key order; we group `rows` by
    /// `mu[var].id()` and emit in btree order, with rows lacking a
    /// btree key (null prop / non-node binding) placed per
    /// `NullsOrder`. Sort cost is O(n + |rows| + k); top-k reduces to
    /// constant work once `k` rows have been emitted.
    fn try_btree_sort(
        &self,
        rows: &mut Vec<ResultRow>,
        specs: &[SortSpec],
        matches: &[MatchStatement],
        limit: usize,
    ) -> bool {
        if specs.len() != 1 {
            return false;
        }
        let spec = &specs[0];
        let SortKey::Expr(Expr::AttrLookup { var, attr }) = &spec.key else {
            return false;
        };
        let labels = labels_for_var(matches, var);
        if labels.is_empty() {
            return false;
        }
        let asc = matches!(spec.dir, SortDir::Asc);
        let ordered_ids = labels
            .iter()
            .find_map(|l| self.graph.lookup_node_ordered(l, attr, asc));
        let Some(ordered_ids) = ordered_ids else {
            return false;
        };
        let nulls_first = matches!(spec.nulls.unwrap_or(NullsOrder::Last), NullsOrder::First);
        let owned = std::mem::take(rows);
        *rows = btree_bucket_output(owned, var, &ordered_ids, limit, nulls_first);
        true
    }

    /// Pre-projection sort over `IntermediateResult` rows. ISO §16.17
    /// GR 1; pdqsort per §16.17 GR 1k/US006 (peer order is
    /// implementation-dependent). Caller guarantees every spec is a
    /// `SortKey::Expr`. `limit = 0` means "no truncation, full sort";
    /// `limit > 0` enables the top-k heap path when the heuristic fires.
    fn sort_rows(&self, rows: &mut Vec<ResultRow>, specs: &[SortSpec], limit: usize) {
        let mut decorated: Vec<(Vec<Option<Value>>, ResultRow)> = std::mem::take(rows)
            .into_iter()
            .map(|row| {
                let keys = specs
                    .iter()
                    .map(|s| match &s.key {
                        SortKey::Expr(e) => match self.run_expr(&row.assignment, e) {
                            ExprResult::Success(Value::Null) | ExprResult::Failure(_) => None,
                            ExprResult::Success(v) => Some(v),
                        },
                        SortKey::Column(_) => unreachable!(
                            "Column sort key reached pre-projection path — caller must \
                             route to sort_projected_rows"
                        ),
                    })
                    .collect();
                (keys, row)
            })
            .collect();

        sort_decorated(&mut decorated, specs, limit);

        rows.extend(decorated.into_iter().map(|(_, r)| r));
    }

    /// Evaluate the match chain. Two paths:
    ///
    /// - All-Simple: collapse to one `PathPattern::Join` and run the
    ///   existing path-pattern evaluator (LTJ + hash-join fallback). No
    ///   behavior change vs. before OPTIONAL existed.
    ///
    /// - Has-Optional: walk matches sequentially. Each Simple is a natural
    ///   join with the accumulated binding table; each Optional is a left
    ///   outer join — for every accumulated row, either emit all unifying
    ///   extensions from the optional pattern, or emit the original row
    ///   padded with `PathValue::Nothing` for new variables (those then
    ///   project as `Value::Null` via the existing AttrLookup-failure path).
    fn run_match_chain(&self, query: &Query, limit: usize) -> IntermediateResult {
        // Keep `Selected` operands isolated; collapsing would erase their
        // per-pattern prefix boundary.
        if !query.has_any_optional() && !query.has_any_selected() {
            self.unbounded_policy.set(UnboundedPolicy::Forbidden);
            return self.run_path_pattern(&query.collapsed_pattern(), limit);
        }

        self.unbounded_policy.set(UnboundedPolicy::Forbidden);

        let mut iter = query.matches.iter();
        let first = iter.next().expect("Query::matches must be non-empty");
        // Only a single, non-selected MATCH can safely push LIMIT inward.
        let first_limit = if query.matches.len() == 1 && !first.pattern().has_selected() {
            limit
        } else {
            0
        };
        let mut acc = self.run_path_pattern(first.pattern(), first_limit);
        let mut bound_vars: HashSet<String> = first.pattern().freevars();

        for m in iter {
            let pattern = m.pattern();
            let new_vars = pattern.freevars();
            acc = match m {
                MatchStatement::Simple { .. } => {
                    let ir_new = self.run_path_pattern(pattern, 0);
                    natural_join(&acc, &ir_new, 0)
                }
                MatchStatement::Optional { .. } => {
                    let pushed = if !pattern.has_selected() {
                        self.optional_via_bind_pushdown(&acc, pattern, &bound_vars, &new_vars)
                    } else {
                        None
                    };
                    pushed.unwrap_or_else(|| {
                        let ir_new = self.run_path_pattern(pattern, 0);
                        left_outer_join(&acc, &ir_new, &bound_vars, &new_vars)
                    })
                }
            };
            bound_vars.extend(new_vars);
            if limit > 0 && acc.rows.len() >= limit {
                acc.rows.truncate(limit);
                break;
            }
        }

        // Defensive truncation: when the for-loop never runs (single-match
        // chain) the LTJ runtime may have produced more than `limit` rows
        // for patterns it can't early-terminate. Without this, a query
        // like `OPTIONAL MATCH (x) LIMIT 5` with no RETURN would emit
        // every binding row instead of five.
        if limit > 0 && acc.rows.len() > limit {
            acc.rows.truncate(limit);
        }

        acc
    }

    /// OPTIONAL MATCH bind-pushdown. The naive path evaluates the inner
    /// pattern globally and then left-outer-joins against the outer rows;
    /// when the inner pattern's hot variables are already bound by the
    /// outer (the `OPTIONAL MATCH (otherPerson)<-[:hasCreator]-(post)<-[:containerOf]-(forum)`
    /// shape over an outer table that already binds `otherPerson` and
    /// `forum`), that global evaluation enumerates a search space orders
    /// of magnitude larger than what survives the join. Per-row LTJ with
    /// the shared variables pinned reduces it to one local intersection
    /// per outer row — analogous to SQLite's correlated nested-loop with
    /// index lookup on the inner side.
    ///
    /// Returns `None` (caller falls back to global eval + left_outer_join) when:
    ///   - the optimization is disabled (`GQLITE_DISABLE_OPTIONAL_PUSHDOWN`);
    ///   - there are no shared variables between outer rows and the inner
    ///     pattern (no correlation to exploit);
    ///   - any outer row binds a shared variable to a non-Node value (edges
    ///     and `Group` repetitions can't be pinned by the LTJ today);
    ///   - the inner pattern is not LTJ-decomposable (unions, repetitions,
    ///     any-direction edges).
    fn optional_via_bind_pushdown(
        &self,
        acc: &IntermediateResult,
        pattern: &PathPattern,
        bound_vars: &HashSet<String>,
        new_vars: &HashSet<String>,
    ) -> Option<IntermediateResult> {
        if std::env::var("GQLITE_DISABLE_OPTIONAL_PUSHDOWN").is_ok() {
            return None;
        }
        let shared: Vec<String> = bound_vars.intersection(new_vars).cloned().collect();
        if shared.is_empty() {
            return None;
        }
        let pad_vars: Vec<String> = new_vars.difference(bound_vars).cloned().collect();
        let index = self.triple_index();

        // Sniff whether the inner pattern is LTJ-decomposable on the first
        // row whose shared bindings are all Node-typed. The decomposition
        // is structural (data-independent), so a single None means the
        // pattern can never be pinned — bail to the global fallback.
        let mut decomposable_checked = false;
        let mut out_rows: Vec<ResultRow> = Vec::with_capacity(acc.rows.len());

        for r1 in &acc.rows {
            let mut pin_pairs: Vec<(&str, u32)> = Vec::with_capacity(shared.len());
            let mut row_pinnable = true;
            for v in &shared {
                match r1.assignment.get(v) {
                    Some(PathValue::Node(id)) => pin_pairs.push((v.as_str(), *id)),
                    _ => {
                        row_pinnable = false;
                        break;
                    }
                }
            }

            if !row_pinnable {
                // Edge / Nothing / Group / unbound: inner can't unify on
                // anything but a Node here, so emit the padded outer row
                // straight away. This preserves the LEFT-OUTER semantics
                // for rows the previous OPTIONAL left as Nothing.
                let mut padded = r1.assignment.clone();
                for v in &pad_vars {
                    padded.extend(v.clone(), PathValue::Nothing);
                }
                out_rows.push(ResultRow::with_paths(r1.paths.clone(), padded));
                continue;
            }

            let inner =
                pattern_extract::try_ltj_with_pins(self.graph, pattern, &index, 0, &pin_pairs)?;
            decomposable_checked = true;

            if inner.rows.is_empty() {
                let mut padded = r1.assignment.clone();
                for v in &pad_vars {
                    padded.extend(v.clone(), PathValue::Nothing);
                }
                out_rows.push(ResultRow::with_paths(r1.paths.clone(), padded));
            } else {
                let mut matched_any = false;
                for r2 in &inner.rows {
                    if r1.assignment.can_unify(&r2.assignment) {
                        matched_any = true;
                        out_rows.push(ResultRow::join(r1, r2, r1.assignment.unify(&r2.assignment)));
                    }
                }
                if !matched_any {
                    let mut padded = r1.assignment.clone();
                    for v in &pad_vars {
                        padded.extend(v.clone(), PathValue::Nothing);
                    }
                    out_rows.push(ResultRow::with_paths(r1.paths.clone(), padded));
                }
            }
        }

        // If the loop never reached `try_ltj_with_pins` (every row had a
        // non-Node shared binding), we never confirmed decomposability —
        // but every row was already emitted as padded, so the result is
        // semantically equivalent to the fallback. Return it.
        let _ = decomposable_checked;
        Some(IntermediateResult::new(out_rows))
    }

    fn run_row_by_row(
        &self,
        items: &[ReturnItem],
        rows: &[ResultRow],
        distinct: bool,
    ) -> Vec<Vec<Value>> {
        let mut projected: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
        // DISTINCT path uses a HashSet via the `GroupKey` wrapper for
        // O(N) dedup. Without this, the naive `projected.contains(&vals)`
        // is O(N) per row → O(N² × m) overall (m = item count). On a
        // 200 k-row IC2 with 6 RETURN items it ran ~38 s; the HashSet
        // path drops it to ~50 ms (~750x). The wrapper exists because
        // `Value` can't implement `Hash` directly — `Float(f64)` makes
        // NaN ≠ NaN and ±0.0 ≠ 0.0 in the derived form.
        let mut seen: HashSet<GroupKey> = if distinct {
            HashSet::with_capacity(rows.len())
        } else {
            HashSet::new()
        };
        for row in rows {
            let vals: Vec<Value> = items
                .iter()
                .map(|item| self.eval_expr_item(item, &row.assignment))
                .collect();
            if distinct {
                let key = GroupKey::from_values(vals.clone());
                if !seen.insert(key) {
                    continue;
                }
            }
            projected.push(vals);
        }
        projected
    }

    /// Evaluate an `Expr`-shaped return item; aggregates require the
    /// group-and-aggregate path and would be a runtime bug here.
    fn eval_expr_item(&self, item: &ReturnItem, mu: &Assignment) -> Value {
        match item {
            ReturnItem::Expr { expr, .. } => match self.run_expr(mu, expr) {
                ExprResult::Success(v) => v,
                ExprResult::Failure(_) => Value::Null,
            },
            ReturnItem::Aggregate { .. } => {
                unreachable!("aggregate items must be projected via run_aggregated")
            }
        }
    }

    /// Group-and-aggregate projection. Keys come from explicit GROUP BY
    /// (ISO §16.15) when present, otherwise from the non-aggregate RETURN
    /// items. ISO §20.9 GR 7a-i: a query with no key items and zero rows
    /// still emits one output row.
    fn run_aggregated(
        &self,
        items: &[ReturnItem],
        explicit_group_by: Option<&[Expr]>,
        rows: &[ResultRow],
    ) -> Vec<Vec<Value>> {
        let mut group_indices: Vec<Vec<usize>> = Vec::new();
        let mut key_to_index: HashMap<GroupKey, usize> = HashMap::new();

        for (row_idx, row) in rows.iter().enumerate() {
            let key_values: Vec<Value> = match explicit_group_by {
                Some(exprs) => exprs
                    .iter()
                    .map(|e| match self.run_expr(&row.assignment, e) {
                        ExprResult::Success(v) => v,
                        ExprResult::Failure(_) => Value::Null,
                    })
                    .collect(),
                None => items
                    .iter()
                    .filter(|it| !it.is_aggregate())
                    .map(|it| self.eval_expr_item(it, &row.assignment))
                    .collect(),
            };
            let key = GroupKey::from_values(key_values);
            match key_to_index.get(&key) {
                Some(&i) => group_indices[i].push(row_idx),
                None => {
                    key_to_index.insert(key, group_indices.len());
                    group_indices.push(vec![row_idx]);
                }
            }
        }

        let key_arity = explicit_group_by
            .map(|e| e.len())
            .unwrap_or_else(|| items.iter().filter(|it| !it.is_aggregate()).count());
        if key_arity == 0 && group_indices.is_empty() {
            group_indices.push(Vec::new());
        }

        let mut out: Vec<Vec<Value>> = Vec::with_capacity(group_indices.len());
        for row_idxs in &group_indices {
            let row_vals: Vec<Value> = items
                .iter()
                .map(|item| match item {
                    // An expression carrying aggregates (`COUNT(x)+COUNT(y)`)
                    // is reduced over the group; a plain key expression is
                    // evaluated on the group's representative row.
                    ReturnItem::Expr { expr, .. } if expr.contains_agg() => {
                        self.eval_grouped_expr(expr, row_idxs, rows)
                    }
                    ReturnItem::Expr { expr, .. } => {
                        let mu = match row_idxs.first() {
                            Some(&i) => &rows[i].assignment,
                            None => return Value::Null,
                        };
                        match self.run_expr(mu, expr) {
                            ExprResult::Success(v) => v,
                            ExprResult::Failure(_) => Value::Null,
                        }
                    }
                    ReturnItem::Aggregate { agg, .. } => self.apply_aggregator(agg, row_idxs, rows),
                })
                .collect();
            out.push(row_vals);
        }
        out
    }

    /// Evaluate a RETURN expression that contains aggregates, per group.
    /// Each `Expr::Agg` node is reduced over the group's rows and folded
    /// to a constant; the residual (now aggregate-free) expression is
    /// evaluated against the group's representative row, so non-aggregate
    /// leaves (`otherPerson.firstName`) resolve to their grouped value.
    fn eval_grouped_expr(&self, expr: &Expr, row_idxs: &[usize], rows: &[ResultRow]) -> Value {
        let folded = self.fold_aggs(expr, row_idxs, rows);
        let empty = Assignment::new();
        let mu = match row_idxs.first() {
            Some(&i) => &rows[i].assignment,
            None => &empty,
        };
        match self.run_expr(mu, &folded) {
            ExprResult::Success(v) => v,
            ExprResult::Failure(_) => Value::Null,
        }
    }

    /// Replace every `Expr::Agg` in the tree with `Expr::Const(reduced)`,
    /// leaving all other nodes structurally intact. The result is an
    /// aggregate-free expression `run_expr` can evaluate directly.
    fn fold_aggs(&self, expr: &Expr, row_idxs: &[usize], rows: &[ResultRow]) -> Expr {
        match expr {
            Expr::Agg(agg) => Expr::Const(self.apply_aggregator(agg, row_idxs, rows)),
            Expr::Binop { op, left, right } => Expr::Binop {
                op: *op,
                left: Box::new(self.fold_aggs(left, row_idxs, rows)),
                right: Box::new(self.fold_aggs(right, row_idxs, rows)),
            },
            Expr::Unop { op, operand } => Expr::Unop {
                op: op.clone(),
                operand: Box::new(self.fold_aggs(operand, row_idxs, rows)),
            },
            Expr::FieldAccess { base, field } => Expr::FieldAccess {
                base: Box::new(self.fold_aggs(base, row_idxs, rows)),
                field: field.clone(),
            },
            Expr::IsNull { operand, negated } => Expr::IsNull {
                operand: Box::new(self.fold_aggs(operand, row_idxs, rows)),
                negated: *negated,
            },
            Expr::Coalesce(args) => Expr::Coalesce(
                args.iter()
                    .map(|a| self.fold_aggs(a, row_idxs, rows))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn apply_aggregator(&self, agg: &Aggregator, row_idxs: &[usize], rows: &[ResultRow]) -> Value {
        match agg {
            // ISO §20.9 GR 2: COUNT(*) is cardinality, no null-elim, no DISTINCT.
            Aggregator::CountStar => Value::Int(row_idxs.len() as i64),
            Aggregator::GeneralSet {
                kind,
                quantifier,
                expr,
            } => {
                let values = self.collect_aggregate_values(expr, *quantifier, row_idxs, rows);
                match kind {
                    GeneralSetKind::Count => Value::Int(values.len() as i64),
                    GeneralSetKind::Sum => sum_values(&values),
                    GeneralSetKind::Avg => avg_values(&values),
                    GeneralSetKind::Min => min_values(&values),
                    GeneralSetKind::Max => max_values(&values),
                }
            }
        }
    }

    /// Evaluate inner expr per row, drop ISO nulls (Failure), optionally
    /// dedup via HashSet when quantifier is DISTINCT.
    fn collect_aggregate_values(
        &self,
        expr: &Expr,
        quantifier: SetQuantifier,
        row_idxs: &[usize],
        rows: &[ResultRow],
    ) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let mut seen: HashSet<GroupKey> = HashSet::new();
        for &idx in row_idxs {
            let v = match self.run_expr(&rows[idx].assignment, expr) {
                ExprResult::Success(Value::Null) => continue, // null-eliminated
                ExprResult::Success(v) => v,
                ExprResult::Failure(_) => continue, // null-eliminated
            };
            if matches!(quantifier, SetQuantifier::Distinct) {
                let key = GroupKey::from_values(vec![v.clone()]);
                if !seen.insert(key) {
                    continue;
                }
            }
            out.push(v);
        }
        out
    }

    fn limit_reached(&self, rows: &[ResultRow], limit: usize) -> bool {
        limit > 0 && rows.len() >= limit
    }

    fn run_path_pattern(&self, p: &PathPattern, limit: usize) -> IntermediateResult {
        match p {
            PathPattern::Node(_) => self.run_node_pattern(p, limit),
            PathPattern::EdgeRight(_)
            | PathPattern::EdgeLeft(_)
            | PathPattern::EdgeUndirected(_)
            | PathPattern::EdgeAnyDirection(_) => self.run_edge_pattern(p),
            PathPattern::Concat(p1, p2) => self.run_concat_pattern(p1, p2, limit),
            PathPattern::Union(p1, p2) => {
                let ir1 = self.run_path_pattern(p1, limit);
                let remaining = if limit > 0 {
                    limit.saturating_sub(ir1.rows.len())
                } else {
                    0
                };
                let ir2 = self.run_path_pattern(p2, remaining);
                let dom = p.freevars();
                let mut rows = Vec::new();
                for mut r in ir1.rows.into_iter().chain(ir2.rows) {
                    r.assignment.fill_nones(&dom);
                    rows.push(r);
                    if self.limit_reached(&rows, limit) {
                        break;
                    }
                }
                IntermediateResult::new(rows)
            }
            PathPattern::Filter(inner, expr) => {
                // For filters, we can't know how many pre-filter results we need,
                // so pass 0 (unlimited) to inner and filter+truncate after.
                let ir = self.run_path_pattern(inner, 0);
                let mut rows = Vec::new();
                for r in ir.rows {
                    if self.run_expr(&r.assignment, expr).get_bool() {
                        rows.push(r);
                        if self.limit_reached(&rows, limit) {
                            break;
                        }
                    }
                }
                IntermediateResult::new(rows)
            }
            PathPattern::Repeat { pattern, lb, ub } => match ub {
                Some(ub) => self.run_repetition_range(pattern, *lb, *ub, limit),
                None => match self.unbounded_policy.get() {
                    // Length-ordered k-shortest search.
                    UnboundedPolicy::Shortest { count, groups } => {
                        self.run_repetition_shortest(pattern, *lb, count, groups, limit)
                    }
                    // Finite enumeration of paths the mode admits.
                    UnboundedPolicy::Mode(mode) => {
                        self.run_repetition_unbounded_mode(pattern, *lb, mode, limit)
                    }
                    // Infinite under WALK/ALL; the typechecker
                    // (`check_unbounded_repetition`) rejects this, so
                    // reaching here is an invariant violation.
                    UnboundedPolicy::Forbidden => panic!(
                        "unbounded repetition reached the runtime without a finite-making \
                         prefix; this must be rejected by check_unbounded_repetition"
                    ),
                },
            },
            PathPattern::Questioned(inner) => {
                let ir_empty = self.run_path_pattern(&PathPattern::Node(None), limit);
                let remaining = if limit > 0 {
                    limit.saturating_sub(ir_empty.rows.len())
                } else {
                    0
                };
                let ir_inner = self.run_path_pattern(inner, remaining);
                ir_empty.union(ir_inner)
            }
            PathPattern::Join(q1, q2) => self.run_join(q1, q2, limit),
            PathPattern::Selected { prefix, pattern } => {
                // Selection ranks the full candidate set, so the inner runs
                // without LIMIT and the caller limit is applied afterwards.
                let prev = self.unbounded_policy.get();
                self.unbounded_policy
                    .set(unbounded_policy_for(Some(*prefix)));
                let ir = self.run_path_pattern(pattern, 0);
                self.unbounded_policy.set(prev);
                let mut selected = apply_path_prefix(ir, *prefix);
                if limit > 0 && selected.rows.len() > limit {
                    selected.rows.truncate(limit);
                }
                selected
            }
        }
    }

    // --- Optimized node pattern: uses label index when available ---

    fn run_node_pattern(&self, p: &PathPattern, limit: usize) -> IntermediateResult {
        let desc = p.descriptor();
        let candidates = self.get_candidate_nodes(desc);
        let var = desc.and_then(|d| d.var.as_deref());

        // Honor `limit` at scan time so a small cap doesn't enumerate
        // the full candidate set. `0` is unbounded per runtime convention.
        let mut rows: Vec<ResultRow> = Vec::new();
        for &id in &candidates {
            if !self.filter_node(id, desc) {
                continue;
            }
            let pv = PathValue::Node(id);
            rows.push(ResultRow::new(
                Path(vec![pv.clone()]),
                Assignment::from_optional(var, pv),
            ));
            if limit > 0 && rows.len() >= limit {
                break;
            }
        }
        IntermediateResult::new(rows)
    }

    /// Get candidate node IDs — uses label index to pick the smallest set.
    fn get_candidate_nodes(&self, desc: Option<&Descriptor>) -> Vec<Id> {
        if let Some(desc) = desc {
            if let Some(best) =
                self.smallest_label_set(&desc.dtype.label, |l| self.graph.nodes_with_label(l))
            {
                return best;
            }
        }
        self.graph.nodes()
    }

    // --- Optimized edge pattern: uses label index when available ---

    fn run_edge_pattern(&self, p: &PathPattern) -> IntermediateResult {
        match p {
            PathPattern::EdgeAnyDirection(d) => {
                let right = self.run_edge_pattern(&PathPattern::EdgeRight(d.clone()));
                let left = self.run_edge_pattern(&PathPattern::EdgeLeft(d.clone()));
                let undirected = self.run_edge_pattern(&PathPattern::EdgeUndirected(d.clone()));
                right.union(left).union(undirected)
            }
            PathPattern::EdgeRight(desc) | PathPattern::EdgeLeft(desc) => {
                let candidates = self.get_candidate_directed_edges(desc.as_ref());
                let is_right = matches!(p, PathPattern::EdgeRight(_));
                let var = desc.as_ref().and_then(|d| d.var.as_deref());

                let rows: Vec<ResultRow> = candidates
                    .iter()
                    .filter(|id| self.filter_edge(**id, desc.as_ref()))
                    .map(|&eid| {
                        let edge_pv = PathValue::EdgeDirectional(eid);
                        let (first, last) = if is_right {
                            (
                                PathValue::Node(self.graph.src(eid)),
                                PathValue::Node(self.graph.tgt(eid)),
                            )
                        } else {
                            (
                                PathValue::Node(self.graph.tgt(eid)),
                                PathValue::Node(self.graph.src(eid)),
                            )
                        };
                        ResultRow::new(
                            Path(vec![first, edge_pv.clone(), last]),
                            Assignment::from_optional(var, edge_pv),
                        )
                    })
                    .collect();
                IntermediateResult::new(rows)
            }
            PathPattern::EdgeUndirected(desc) => {
                let candidates = self.get_candidate_undirected_edges(desc.as_ref());
                let var = desc.as_ref().and_then(|d| d.var.as_deref());

                let mut rows = Vec::new();
                for &eid in candidates
                    .iter()
                    .filter(|id| self.filter_edge(**id, desc.as_ref()))
                {
                    let edge_pv = PathValue::EdgeUndirectional(eid);
                    let ep0 = self.graph.src(eid);
                    let ep1 = self.graph.tgt(eid);
                    rows.push(ResultRow::new(
                        Path(vec![
                            PathValue::Node(ep0),
                            edge_pv.clone(),
                            PathValue::Node(ep1),
                        ]),
                        Assignment::from_optional(var, edge_pv.clone()),
                    ));
                    rows.push(ResultRow::new(
                        Path(vec![
                            PathValue::Node(ep1),
                            edge_pv.clone(),
                            PathValue::Node(ep0),
                        ]),
                        Assignment::from_optional(var, edge_pv),
                    ));
                }
                IntermediateResult::new(rows)
            }
            _ => unreachable!(),
        }
    }

    fn get_candidate_directed_edges(&self, desc: Option<&Descriptor>) -> Vec<Id> {
        if let Some(desc) = desc {
            if let Some(best) = self.smallest_label_set(&desc.dtype.label, |l| {
                self.graph.directed_edges_with_label(l)
            }) {
                return best;
            }
        }
        self.graph.edges_directed()
    }

    fn get_candidate_undirected_edges(&self, desc: Option<&Descriptor>) -> Vec<Id> {
        if let Some(desc) = desc {
            if let Some(best) = self.smallest_label_set(&desc.dtype.label, |l| {
                self.graph.undirected_edges_with_label(l)
            }) {
                return best;
            }
        }
        self.graph.edges_undirected()
    }

    // --- Join: Q1, Q2 — cross-product with assignment unification ---

    fn run_join(&self, q1: &PathPattern, q2: &PathPattern, limit: usize) -> IntermediateResult {
        // Try LTJ for multi-way joins. The TripleIndex is shared via Arc;
        // built once per Runtime (or supplied by the connection that built
        // it at open) and reused across every subsequent query.
        let join_pattern = PathPattern::Join(Box::new(q1.clone()), Box::new(q2.clone()));
        let index = self.triple_index();
        if let Some(result) = pattern_extract::try_ltj(self.graph, &join_pattern, &index, limit) {
            return result;
        }

        // Fallback to pairwise hash-join
        let ir1 = self.run_path_pattern(q1, 0);
        let ir2 = self.run_path_pattern(q2, 0);

        let shared_vars: Vec<String> = {
            let fv1 = q1.freevars();
            let fv2 = q2.freevars();
            fv1.intersection(&fv2).cloned().collect()
        };

        // If there are shared variables, build a hash index on ir2 for efficiency
        if let Some(join_var) = shared_vars.first() {
            let mut ir2_by_val: HashMap<&PathValue, Vec<usize>> = HashMap::new();
            for (i, r2) in ir2.rows.iter().enumerate() {
                if let Some(pv) = r2.assignment.get(join_var) {
                    ir2_by_val.entry(pv).or_default().push(i);
                }
            }

            let mut rows = Vec::new();
            'outer: for r1 in &ir1.rows {
                if let Some(pv) = r1.assignment.get(join_var) {
                    if let Some(indices) = ir2_by_val.get(pv) {
                        for &idx in indices {
                            let r2 = &ir2.rows[idx];
                            if r1.assignment.can_unify(&r2.assignment) {
                                rows.push(ResultRow::join(
                                    r1,
                                    r2,
                                    r1.assignment.unify(&r2.assignment),
                                ));
                                if self.limit_reached(&rows, limit) {
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
            }
            return IntermediateResult::new(rows);
        }

        // No shared variables: full cross-product
        let mut rows = Vec::new();
        'outer2: for r1 in &ir1.rows {
            for r2 in &ir2.rows {
                rows.push(ResultRow::join(r1, r2, r1.assignment.unify(&r2.assignment)));
                if self.limit_reached(&rows, limit) {
                    break 'outer2;
                }
            }
        }
        IntermediateResult::new(rows)
    }

    // --- Optimized concatenation: uses adjacency when right side is edge/node ---

    fn run_concat_pattern(
        &self,
        p1: &PathPattern,
        p2: &PathPattern,
        limit: usize,
    ) -> IntermediateResult {
        // Try LTJ for chains of directed edges. Cached TripleIndex via Arc.
        let concat_pattern = PathPattern::Concat(Box::new(p1.clone()), Box::new(p2.clone()));
        let index = self.triple_index();
        if let Some(result) = pattern_extract::try_ltj(self.graph, &concat_pattern, &index, limit) {
            return result;
        }

        let ir1 = self.run_path_pattern(p1, 0);

        // Optimization: if p2 is a simple edge or node pattern, use adjacency-driven execution
        match p2 {
            PathPattern::EdgeRight(desc) => {
                return self.concat_with_directed_edge(&ir1, desc.as_ref(), true, limit);
            }
            PathPattern::EdgeLeft(desc) => {
                return self.concat_with_directed_edge(&ir1, desc.as_ref(), false, limit);
            }
            PathPattern::EdgeUndirected(desc) => {
                return self.concat_with_undirected_edge(&ir1, desc.as_ref(), limit);
            }
            PathPattern::EdgeAnyDirection(desc) => {
                let right = self.concat_with_directed_edge(&ir1, desc.as_ref(), true, limit);
                if self.limit_reached(&right.rows, limit) {
                    return right;
                }
                let remaining = if limit > 0 {
                    limit - right.rows.len()
                } else {
                    0
                };
                let left = self.concat_with_directed_edge(&ir1, desc.as_ref(), false, remaining);
                let combined = right.union(left);
                if self.limit_reached(&combined.rows, limit) {
                    return combined;
                }
                let remaining = if limit > 0 {
                    limit - combined.rows.len()
                } else {
                    0
                };
                let und = self.concat_with_undirected_edge(&ir1, desc.as_ref(), remaining);
                return combined.union(und);
            }
            PathPattern::Node(desc) => {
                return self.concat_with_node(&ir1, desc.as_ref(), limit);
            }
            // For Filter wrapping an edge pattern, we can still optimize the edge part
            PathPattern::Filter(inner, expr) => {
                if let Some(optimized) =
                    self.try_concat_with_filtered_edge(&ir1, inner, expr, limit)
                {
                    return optimized;
                }
            }
            _ => {}
        }

        // Fallback: cross-product for complex right-side patterns
        let ir2 = self.run_path_pattern(p2, 0);
        Self::hash_join(&ir1, &ir2, limit)
    }

    /// Adjacency-driven concat: left results → outgoing/incoming edges → target nodes.
    fn concat_with_directed_edge(
        &self,
        ir1: &IntermediateResult,
        desc: Option<&Descriptor>,
        is_right: bool,
        limit: usize,
    ) -> IntermediateResult {
        let var = desc.and_then(|d| d.var.as_deref());
        let mut rows = Vec::new();

        'outer: for r1 in &ir1.rows {
            let Some(last_node) = r1.path().last_node_id() else {
                continue;
            };

            let edge_ids = if is_right {
                self.graph.outgoing_edges(last_node)
            } else {
                self.graph.incoming_edges(last_node)
            };

            for &eid in &edge_ids {
                if !self.filter_edge(eid, desc) {
                    continue;
                }
                let edge_pv = PathValue::EdgeDirectional(eid);
                let other_node = if is_right {
                    self.graph.tgt(eid)
                } else {
                    self.graph.src(eid)
                };
                let edge_mu = Assignment::from_optional(var, edge_pv.clone());
                if !r1.assignment.can_unify(&edge_mu) {
                    continue;
                }
                rows.push(r1.extend_path(
                    &Path(vec![
                        PathValue::Node(last_node),
                        edge_pv,
                        PathValue::Node(other_node),
                    ]),
                    r1.assignment.unify(&edge_mu),
                ));
                if self.limit_reached(&rows, limit) {
                    break 'outer;
                }
            }
        }
        IntermediateResult::new(rows)
    }

    fn concat_with_undirected_edge(
        &self,
        ir1: &IntermediateResult,
        desc: Option<&Descriptor>,
        limit: usize,
    ) -> IntermediateResult {
        let var = desc.and_then(|d| d.var.as_deref());
        let mut rows = Vec::new();

        'outer: for r1 in &ir1.rows {
            let Some(last_node) = r1.path().last_node_id() else {
                continue;
            };

            for &eid in &self.graph.undirected_edges_of(last_node) {
                if !self.filter_edge(eid, desc) {
                    continue;
                }
                let edge_pv = PathValue::EdgeUndirectional(eid);
                let ep0 = self.graph.src(eid);
                let ep1 = self.graph.tgt(eid);
                let other_node = if ep0 == last_node { ep1 } else { ep0 };

                let edge_mu = Assignment::from_optional(var, edge_pv.clone());
                if !r1.assignment.can_unify(&edge_mu) {
                    continue;
                }
                rows.push(r1.extend_path(
                    &Path(vec![
                        PathValue::Node(last_node),
                        edge_pv,
                        PathValue::Node(other_node),
                    ]),
                    r1.assignment.unify(&edge_mu),
                ));
                // Second orientation: other_node → edge → last_node
                // But wait — the edge is attached to last_node already.
                // For undirected edges in run_edge_pattern, we emit both orientations.
                // Here we only emit the one where last_node is an endpoint (which it is).
                // We need both orientations: last→other AND last→other reversed
                // Actually no — the path starts from last_node, so we only produce
                // last_node → edge → other_node. The other orientation would mean
                // the path goes from last_node through the same edge to the same nodes,
                // just labeling the endpoints differently. But since last_node is fixed,
                // we get: last_node, edge, other_node.
                // For the case where both endpoints are relevant, undirected_edges_of
                // returns the edge for BOTH endpoints, so if a path can reach either
                // endpoint, both are handled.
                if self.limit_reached(&rows, limit) {
                    break 'outer;
                }
            }
        }
        IntermediateResult::new(rows)
    }

    /// Adjacency-driven concat with a node pattern: just check if the last node matches.
    fn concat_with_node(
        &self,
        ir1: &IntermediateResult,
        desc: Option<&Descriptor>,
        limit: usize,
    ) -> IntermediateResult {
        let var = desc.and_then(|d| d.var.as_deref());
        let mut rows = Vec::new();

        for r1 in &ir1.rows {
            let Some(last_node) = r1.path().last_node_id() else {
                continue;
            };
            if !self.filter_node(last_node, desc) {
                continue;
            }
            let node_pv = PathValue::Node(last_node);
            let node_mu = Assignment::from_optional(var, node_pv);
            if !r1.assignment.can_unify(&node_mu) {
                continue;
            }
            // Node concat: the last node already IS the node, so path doesn't grow
            rows.push(r1.with_same_paths(r1.assignment.unify(&node_mu)));
            if self.limit_reached(&rows, limit) {
                break;
            }
        }
        IntermediateResult::new(rows)
    }

    /// Try to optimize concat with Filter(edge_pattern, expr).
    fn try_concat_with_filtered_edge(
        &self,
        ir1: &IntermediateResult,
        inner: &PathPattern,
        expr: &Expr,
        limit: usize,
    ) -> Option<IntermediateResult> {
        match inner {
            PathPattern::EdgeRight(desc) => {
                let ir = self.concat_with_directed_edge(ir1, desc.as_ref(), true, 0);
                Some(self.apply_filter(ir, expr, limit))
            }
            PathPattern::EdgeLeft(desc) => {
                let ir = self.concat_with_directed_edge(ir1, desc.as_ref(), false, 0);
                Some(self.apply_filter(ir, expr, limit))
            }
            PathPattern::EdgeUndirected(desc) => {
                let ir = self.concat_with_undirected_edge(ir1, desc.as_ref(), 0);
                Some(self.apply_filter(ir, expr, limit))
            }
            PathPattern::EdgeAnyDirection(desc) => {
                let r = self.concat_with_directed_edge(ir1, desc.as_ref(), true, 0);
                let l = self.concat_with_directed_edge(ir1, desc.as_ref(), false, 0);
                let u = self.concat_with_undirected_edge(ir1, desc.as_ref(), 0);
                Some(self.apply_filter(r.union(l).union(u), expr, limit))
            }
            _ => None,
        }
    }

    fn apply_filter(
        &self,
        ir: IntermediateResult,
        expr: &Expr,
        limit: usize,
    ) -> IntermediateResult {
        let mut rows = Vec::new();
        for r in ir.rows {
            if self.run_expr(&r.assignment, expr).get_bool() {
                rows.push(r);
                if self.limit_reached(&rows, limit) {
                    break;
                }
            }
        }
        IntermediateResult::new(rows)
    }

    /// Hash-join on the concatenation key (last node of ir1 = first node of ir2).
    /// O(n + m) expected instead of O(n × m) cross-product.
    fn hash_join(
        ir1: &IntermediateResult,
        ir2: &IntermediateResult,
        limit: usize,
    ) -> IntermediateResult {
        // Build hash map: first_node_id → Vec<index into ir2.rows>
        let mut ir2_by_first: HashMap<Id, Vec<usize>> = HashMap::new();
        for (i, r2) in ir2.rows.iter().enumerate() {
            if let Some(first) = r2.path().first_node_id() {
                ir2_by_first.entry(first).or_default().push(i);
            }
        }

        let mut rows = Vec::new();
        'outer: for r1 in &ir1.rows {
            let Some(last) = r1.path().last_node_id() else {
                continue;
            };
            let Some(matches) = ir2_by_first.get(&last) else {
                continue;
            };
            for &idx in matches {
                let r2 = &ir2.rows[idx];
                if r1.assignment.can_unify(&r2.assignment) {
                    rows.push(r1.extend_path(r2.path(), r1.assignment.unify(&r2.assignment)));
                    if limit > 0 && rows.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
        IntermediateResult::new(rows)
    }

    fn run_repetition_pattern(&self, p: &PathPattern, n: usize) -> IntermediateResult {
        if n == 0 {
            let mut mu = Assignment::new();
            mu.fill_empty_list(&p.freevars());
            let all_nodes = self.graph.nodes();
            let rows: Vec<ResultRow> = all_nodes
                .iter()
                .map(|&id| ResultRow::new(Path(vec![PathValue::Node(id)]), mu.clone()))
                .collect();
            return IntermediateResult::new(rows);
        }

        let ir = self.run_path_pattern(p, 0);
        let grouped = ir.to_group();

        if n == 1 {
            return grouped;
        }

        // Build hash map once: first_node_id → indices in grouped.rows
        let mut grouped_by_first: HashMap<Id, Vec<usize>> = HashMap::new();
        for (i, r) in grouped.rows.iter().enumerate() {
            if let Some(first) = r.path().first_node_id() {
                grouped_by_first.entry(first).or_default().push(i);
            }
        }

        let mut res = grouped.clone();
        for _ in 1..n {
            let mut new_rows = Vec::new();
            for r in &res.rows {
                let Some(last) = r.path().last_node_id() else {
                    continue;
                };
                let Some(matches) = grouped_by_first.get(&last) else {
                    continue;
                };
                for &idx in matches {
                    new_rows.push(r.concat_group(&grouped.rows[idx]));
                }
            }
            res = IntermediateResult::new(new_rows);
        }
        res
    }

    /// Range-incremental repetition. Evaluates the inner pattern once,
    /// builds the first→indices hash map once, then walks every level
    /// 1..=ub appending rows directly to a single `acc` vector. The
    /// previous level's row range is reused (by index) as the source
    /// for the next level, so no level beyond level 1 is ever cloned —
    /// each row lives in `acc` exactly once. Levels below `lb` are
    /// built in-place and drained at the end via a single `Vec::drain`
    /// (one O(remaining) memmove). For `lb == ub` it does the same
    /// work as legacy minus 1 inner eval; for `lb < ub` it replaces the
    /// legacy O((ub-lb+1) * ub) build work with O(ub).
    fn run_repetition_range(
        &self,
        p: &PathPattern,
        lb: usize,
        ub: usize,
        limit: usize,
    ) -> IntermediateResult {
        if lb > ub {
            return IntermediateResult::empty();
        }

        // Length 0 is a special case: a row per node, with empty-list
        // bindings. Reuse the legacy n=0 codepath rather than
        // duplicating the all-nodes scan.
        let mut prefix_rows = if lb == 0 {
            self.run_repetition_pattern(p, 0).rows
        } else {
            Vec::new()
        };

        if ub == 0 {
            if limit > 0 && prefix_rows.len() > limit {
                prefix_rows.truncate(limit);
            }
            return IntermediateResult::new(prefix_rows);
        }
        if limit > 0 && prefix_rows.len() >= limit {
            prefix_rows.truncate(limit);
            return IntermediateResult::new(prefix_rows);
        }

        let grouped = self.run_path_pattern(p, 0).to_group();
        if grouped.rows.is_empty() {
            return IntermediateResult::new(prefix_rows);
        }

        let mut grouped_by_first: HashMap<Id, Vec<usize>> = HashMap::new();
        for (i, r) in grouped.rows.iter().enumerate() {
            if let Some(first) = r.path().first_node_id() {
                grouped_by_first.entry(first).or_default().push(i);
            }
        }

        // Single buffer for levels 1..=ub. level_ranges[k-1] = (start, end)
        // points to level k inside `rows`. The previous level's slice is
        // borrowed by index when building the next.
        let mut rows: Vec<ResultRow> = Vec::new();
        let mut level_ranges: Vec<(usize, usize)> = Vec::with_capacity(ub);

        // Level 1: cloned once from `grouped` (small — just the inner
        // pattern's result table). Subsequent levels live in `rows` only.
        rows.extend(grouped.rows.iter().cloned());
        level_ranges.push((0, rows.len()));

        for k in 2..=ub {
            let (prev_start, prev_end) = level_ranges[k - 2];
            if prev_start == prev_end {
                break;
            }
            let cur_start = rows.len();
            for i in prev_start..prev_end {
                let Some(last) = rows[i].path().last_node_id() else {
                    continue;
                };
                if let Some(matches) = grouped_by_first.get(&last) {
                    // `matches` borrows from grouped_by_first (HashMap),
                    // which is independent of `rows` — no borrow conflict.
                    // Each `concat_group` call is a short borrow of rows[i]
                    // that ends before the subsequent `rows.push`.
                    for &idx in matches {
                        let new_row = rows[i].concat_group(&grouped.rows[idx]);
                        rows.push(new_row);
                    }
                }
            }
            level_ranges.push((cur_start, rows.len()));

            // Early termination on limit (counts only levels >= lb that
            // have been built so far; we approximate by total rows minus
            // the soon-to-be-drained prefix).
            if limit > 0 {
                let drained_so_far: usize = level_ranges
                    .iter()
                    .take(lb.saturating_sub(1).min(level_ranges.len()))
                    .map(|(s, e)| e - s)
                    .sum();
                if rows.len().saturating_sub(drained_so_far) >= limit {
                    break;
                }
            }
        }

        // Drop levels [1, lb-1] from rows by draining the contiguous
        // prefix they occupy. Single memmove of the remaining tail.
        if lb > 1 {
            let drop_levels = (lb - 1).min(level_ranges.len());
            let drop_until = level_ranges
                .get(drop_levels - 1)
                .map(|(_, e)| *e)
                .unwrap_or(0);
            if drop_until > 0 {
                rows.drain(0..drop_until);
            }
        }

        // Splice the optional length-0 baseline at the front.
        if !prefix_rows.is_empty() {
            prefix_rows.extend(rows);
            rows = prefix_rows;
        }

        if limit > 0 && rows.len() > limit {
            rows.truncate(limit);
        }
        IntermediateResult::new(rows)
    }

    /// Unbounded repetition (`*` → `lb == 0`, `+` → `lb == 1`) under a
    /// SHORTEST-family search in WALK mode: the `count` shortest paths
    /// (`groups == false`) or the paths in the `count` shortest length
    /// groups (`groups == true`) per boundary pair `(first, last)`.
    ///
    /// **Why it terminates on cycles.** Under WALK an unbounded repeat is
    /// infinite (every extra lap is another match), but the k-shortest
    /// answer is finite: a length-ordered search admits at most `count`
    /// paths (or length groups) per boundary pair and then prunes, and on
    /// a cycle the per-pair lengths strictly grow so every reachable pair
    /// fills its quota in finitely many steps.
    ///
    /// **Why pruning is sound (optimal substructure).** If a path π to
    /// boundary pair `(s, u)` has a length among `u`'s `count` shortest,
    /// then its prefix π′ to the predecessor pair `(s, t)` also has a
    /// length among `t`'s `count` shortest: otherwise there would be ≥
    /// `count` strictly shorter prefixes to `t`, each extending to a
    /// strictly shorter path to `u`, contradicting π's rank. So a path
    /// whose pair is already full can never be the prefix of a wanted
    /// path and is dropped without expansion — for both PATHS (admit ≤
    /// `count` paths) and GROUPS (admit ≤ `count` distinct lengths). A
    /// min-heap on length feeds paths in non-decreasing order, so this
    /// per-pair budgeting is exact.
    ///
    /// `lb <= 1` is guaranteed by the caller (the typechecker routes
    /// `{n,}` with `n >= 2` to a restrictive mode instead).
    fn run_repetition_shortest(
        &self,
        p: &PathPattern,
        lb: usize,
        count: usize,
        groups: bool,
        limit: usize,
    ) -> IntermediateResult {
        // Admit `pair` at `len` against the per-pair budget, recording it.
        // Returns whether the path is wanted (and so worth expanding).
        fn admit(
            groups: bool,
            count: usize,
            taken: &mut HashMap<(Id, Id), usize>,
            lens: &mut HashMap<(Id, Id), Vec<usize>>,
            pair: (Id, Id),
            len: usize,
        ) -> bool {
            if groups {
                let v = lens.entry(pair).or_default();
                if v.contains(&len) {
                    return true; // another path in an already-counted group
                }
                if v.len() < count {
                    v.push(len);
                    return true; // a new (still within budget) length group
                }
                false
            } else {
                let c = taken.entry(pair).or_insert(0);
                if *c < count {
                    *c += 1;
                    return true;
                }
                false
            }
        }

        // `*` admits the length-0 match: one row per node (a == b), the
        // shortest possible path for every self pair.
        let mut result: Vec<ResultRow> = if lb == 0 {
            self.run_repetition_pattern(p, 0).rows
        } else {
            Vec::new()
        };

        let grouped = self.run_path_pattern(p, 0).to_group();
        if grouped.rows.is_empty() {
            if limit > 0 && result.len() > limit {
                result.truncate(limit);
            }
            return IntermediateResult::new(result);
        }

        let mut grouped_by_first: HashMap<Id, Vec<usize>> = HashMap::new();
        for (i, r) in grouped.rows.iter().enumerate() {
            if let Some(first) = r.path().first_node_id() {
                grouped_by_first.entry(first).or_default().push(i);
            }
        }

        let mut taken: HashMap<(Id, Id), usize> = HashMap::new();
        let mut lens: HashMap<(Id, Id), Vec<usize>> = HashMap::new();

        // The length-0 self matches occupy each self pair's first budget
        // slot, so a 1-lap cycle back to the start competes for what
        // remains (and is dropped under `count == 1`).
        if lb == 0 {
            for r in &result {
                if let Some(n) = r.path().first_node_id() {
                    admit(groups, count, &mut taken, &mut lens, (n, n), 0);
                }
            }
        }

        // Seed the heap with the single inner-pattern applications.
        let mut heap: BinaryHeap<ShortestEntry> = BinaryHeap::new();
        let mut seq = 0usize;
        for r in &grouped.rows {
            heap.push(ShortestEntry {
                len: path_edge_len(r.path()),
                seq,
                reps: 1,
                row: r.clone(),
            });
            seq += 1;
        }

        while let Some(entry) = heap.pop() {
            if limit > 0 && result.len() >= limit {
                break;
            }
            let (Some(f), Some(l)) = (
                entry.row.path().first_node_id(),
                entry.row.path().last_node_id(),
            ) else {
                continue;
            };
            if !admit(groups, count, &mut taken, &mut lens, (f, l), entry.len) {
                continue;
            }
            if entry.reps >= lb {
                result.push(entry.row.clone());
            }
            if let Some(idxs) = grouped_by_first.get(&l) {
                for &idx in idxs {
                    let new_row = entry.row.concat_group(&grouped.rows[idx]);
                    heap.push(ShortestEntry {
                        len: path_edge_len(new_row.path()),
                        seq,
                        reps: entry.reps + 1,
                        row: new_row,
                    });
                    seq += 1;
                }
            }
        }

        if limit > 0 && result.len() > limit {
            result.truncate(limit);
        }
        IntermediateResult::new(result)
    }

    /// Enumerate unbounded repetition under a finite restrictive mode.
    fn run_repetition_unbounded_mode(
        &self,
        p: &PathPattern,
        lb: usize,
        mode: PathMode,
        limit: usize,
    ) -> IntermediateResult {
        // `*` admits the length-0 match (a lone node), which trivially
        // satisfies every mode.
        let mut result: Vec<ResultRow> = if lb == 0 {
            self.run_repetition_pattern(p, 0).rows
        } else {
            Vec::new()
        };

        let grouped = self.run_path_pattern(p, 0).to_group();
        if grouped.rows.is_empty() {
            if limit > 0 && result.len() > limit {
                result.truncate(limit);
            }
            return IntermediateResult::new(result);
        }

        let mut grouped_by_first: HashMap<Id, Vec<usize>> = HashMap::new();
        for (i, r) in grouped.rows.iter().enumerate() {
            if let Some(first) = r.path().first_node_id() {
                grouped_by_first.entry(first).or_default().push(i);
            }
        }

        // Worklist of (partial path, repetition count). Seeded with the
        // mode-satisfying single applications (level 1).
        let mut worklist: Vec<(ResultRow, usize)> = Vec::new();
        for r in &grouped.rows {
            if path_satisfies_mode(r.path(), mode) {
                if lb <= 1 {
                    result.push(r.clone());
                }
                worklist.push((r.clone(), 1));
            }
        }

        while let Some((row, reps)) = worklist.pop() {
            if limit > 0 && result.len() >= limit {
                break;
            }
            let Some(last) = row.path().last_node_id() else {
                continue;
            };
            let Some(idxs) = grouped_by_first.get(&last) else {
                continue;
            };
            for &idx in idxs {
                let new_row = row.concat_group(&grouped.rows[idx]);
                // Pruning on the *whole* extended path is what guarantees
                // termination: a path that already repeats a node/edge is
                // dropped and never extended further.
                if path_satisfies_mode(new_row.path(), mode) {
                    let new_reps = reps + 1;
                    if new_reps >= lb {
                        result.push(new_row.clone());
                    }
                    worklist.push((new_row, new_reps));
                }
            }
        }

        if limit > 0 && result.len() > limit {
            result.truncate(limit);
        }
        IntermediateResult::new(result)
    }

    // --- Helpers ---

    /// From all required labels in a label type, find the one with the smallest
    /// indexed set. Returns None if no label has an index entry.
    fn smallest_label_set(
        &self,
        label: &LabelType,
        lookup: impl Fn(&str) -> Option<Vec<Id>>,
    ) -> Option<Vec<Id>> {
        let required = label.required_labels();
        if required.is_empty() {
            return None;
        }
        required
            .iter()
            .filter_map(|l| lookup(l))
            .min_by_key(|v| v.len())
    }

    fn filter_node(&self, id: Id, desc: Option<&Descriptor>) -> bool {
        match desc {
            None => true,
            Some(d) => {
                let raw_props = self.graph.node_props(id);
                let actual_label = self.graph.node_labels(id);
                let actual_props = Self::check_record(&raw_props);
                let actual = DescriptorType::new(actual_label.clone(), actual_props);
                if !DescriptorType::is_subtype(&actual, &d.dtype) {
                    return false;
                }
                check_value_preds(&d.value_preds, &raw_props)
            }
        }
    }

    fn filter_edge(&self, id: Id, desc: Option<&Descriptor>) -> bool {
        match desc {
            None => true,
            Some(d) => {
                let raw_props = self.graph.edge_props(id);
                let actual_label = self.graph.edge_labels(id);
                let actual_props = Self::check_record(&raw_props);
                let actual = DescriptorType::new(actual_label.clone(), actual_props);
                if !DescriptorType::is_subtype(&actual, &d.dtype) {
                    return false;
                }
                check_value_preds(&d.value_preds, &raw_props)
            }
        }
    }

    fn check_record(props: &Props) -> PropertyType {
        let mut m = std::collections::BTreeMap::new();
        for (k, v) in props {
            m.insert(k.clone(), Self::value_type(v));
        }
        PropertyType::Closed(m)
    }

    /// Inferred SimpleType of a runtime Value. Empty lists widen to `List(Star)`
    /// since the element type is unobservable.
    fn value_type(v: &Value) -> SimpleType {
        match v {
            Value::Null => SimpleType::Zero,
            Value::Int(_) => SimpleType::Z,
            Value::Float(_) => SimpleType::F,
            Value::Str(_) => SimpleType::S,
            Value::Bool(_) => SimpleType::B,
            Value::List(items) => {
                if items.is_empty() {
                    SimpleType::List(Box::new(SimpleType::Star))
                } else {
                    let mut acc = SimpleType::Zero;
                    for it in items {
                        acc = SimpleType::union(&acc, &Self::value_type(it));
                    }
                    SimpleType::List(Box::new(acc))
                }
            }
            Value::Record(fields) => {
                let mut m = std::collections::BTreeMap::new();
                for (k, v) in fields {
                    m.insert(k.clone(), Self::value_type(v));
                }
                SimpleType::Record(m)
            }
            Value::Node(_) => SimpleType::Node,
            Value::Edge(_) => SimpleType::Edge,
        }
    }

    /// Evaluate an `Expr` against a binding row. Public so the DML
    /// runtime (`runtime::dm`) can resolve `INSERT (b {who: a.name})`-
    /// style expressions in MVP-1.
    pub fn run_expr(&self, mu: &Assignment, expr: &Expr) -> ExprResult {
        match expr {
            Expr::Const(v) => ExprResult::Success(v.clone()),

            // ISO §20.12 + §4.4.4: a `<binding variable reference>`
            // resolves to a reference value (Node/Edge by id), or to
            // null when the variable is bound to `PathValue::Nothing`
            // (an OPTIONAL match that did not fire). Failure is
            // reserved for repetition-grouping and unbound names.
            Expr::Var(name) => match mu.get(name) {
                Some(pv) => ExprResult::Success(path_value_to_value(pv)),
                None => ExprResult::Failure(format!("variable '{name}' not bound")),
            },

            Expr::AttrLookup { var, attr } => {
                let pv = match mu.get(var) {
                    Some(pv) => pv,
                    None => return ExprResult::Failure(format!("variable '{var}' not bound")),
                };
                let id = match pv.id() {
                    Some(id) => id,
                    None => return ExprResult::Failure(format!("variable '{var}' has no id")),
                };
                let props = if pv.is_node() {
                    self.graph.node_props(id)
                } else {
                    self.graph.edge_props(id)
                };
                match props.get(attr) {
                    Some(v) => ExprResult::Success(v.clone()),
                    None => ExprResult::Failure(format!("attribute '{attr}' not found")),
                }
            }

            Expr::FieldAccess { base, field } => match self.run_expr(mu, base) {
                ExprResult::Success(Value::Record(m)) => match m.get(field) {
                    Some(v) => ExprResult::Success(v.clone()),
                    None => ExprResult::Failure(format!("field '{field}' not found")),
                },
                ExprResult::Success(other) => {
                    ExprResult::Failure(format!("field access on non-record value: {other}"))
                }
                e @ ExprResult::Failure(_) => e,
            },

            Expr::Binop { op, left, right } => match op {
                BinOp::Is => {
                    let l = self.run_expr(mu, left);
                    match (&l, right.as_ref()) {
                        (ExprResult::Success(val), Expr::Type(ty)) => {
                            ExprResult::Success(Value::Bool(Expr::value_is_type(val, ty)))
                        }
                        (ExprResult::Failure(_), _) => l,
                        _ => ExprResult::Failure("invalid 'is' operands".into()),
                    }
                }
                BinOp::As => {
                    let l = self.run_expr(mu, left);
                    match (&l, right.as_ref()) {
                        (ExprResult::Success(val), Expr::Type(ty)) => {
                            if Expr::value_is_type(val, ty) {
                                ExprResult::Success(val.clone())
                            } else {
                                ExprResult::Failure(format!("cannot cast {val} to {ty}"))
                            }
                        }
                        (ExprResult::Failure(_), _) => l,
                        _ => ExprResult::Failure("invalid 'as' operands".into()),
                    }
                }
                _ => {
                    let (ty_l, _, _) = op.delta(&SimpleType::Star, &SimpleType::Star);
                    let cast_left = Expr::Binop {
                        op: BinOp::As,
                        left: left.clone(),
                        right: Box::new(Expr::Type(ty_l.clone())),
                    };
                    let cast_right = Expr::Binop {
                        op: BinOp::As,
                        left: right.clone(),
                        right: Box::new(Expr::Type(ty_l)),
                    };
                    let l = self.run_expr(mu, &cast_left);
                    let r = self.run_expr(mu, &cast_right);
                    match (&l, &r) {
                        (ExprResult::Success(lv), ExprResult::Success(rv)) => {
                            Self::eval_binop(op, lv, rv)
                        }
                        (ExprResult::Failure(_), _) => l,
                        (_, ExprResult::Failure(_)) => r,
                    }
                }
            },

            Expr::Unop { op, operand } => {
                let (expected_ty, _) = op.delta();
                let cast = Expr::Binop {
                    op: BinOp::As,
                    left: operand.clone(),
                    right: Box::new(Expr::Type(expected_ty)),
                };
                let r = self.run_expr(mu, &cast);
                match &r {
                    ExprResult::Success(val) => match op {
                        UnOp::Neg => match val {
                            Value::Int(n) => ExprResult::Success(Value::Int(-n)),
                            Value::Float(x) => ExprResult::Success(Value::Float(-x)),
                            _ => ExprResult::Failure("neg requires int or float".into()),
                        },
                        UnOp::Not => match val {
                            Value::Bool(b) => ExprResult::Success(Value::Bool(!b)),
                            _ => ExprResult::Failure("not requires bool".into()),
                        },
                    },
                    ExprResult::Failure(_) => r,
                }
            }

            Expr::IsNull { operand, negated } => {
                let is_null = match self.run_expr(mu, operand) {
                    ExprResult::Success(Value::Null) => true,
                    ExprResult::Success(_) => false,
                    // Failure (missing attribute, unbound variable) is
                    // treated as null — the same convention as the rest
                    // of the engine.
                    ExprResult::Failure(_) => true,
                };
                ExprResult::Success(Value::Bool(if *negated { !is_null } else { is_null }))
            }

            Expr::Coalesce(args) => {
                // ISO §20.7 SR 1c-1d: first non-null wins. Failure is
                // treated as null (3VL).
                for a in args {
                    match self.run_expr(mu, a) {
                        ExprResult::Success(Value::Null) | ExprResult::Failure(_) => continue,
                        ExprResult::Success(v) => return ExprResult::Success(v),
                    }
                }
                ExprResult::Success(Value::Null)
            }

            Expr::Type(_) => ExprResult::Failure("bare type in expression".into()),

            Expr::Exists { body } => self.eval_exists(mu, body, /*negated=*/ false),
            Expr::NotExists { body } => self.eval_exists(mu, body, /*negated=*/ true),

            // An aggregate has no meaning against a single binding; it is
            // reduced over a group in `run_aggregated`, which folds every
            // `Agg` node to a constant before calling `run_expr`. Reaching
            // here means an aggregate appeared outside an aggregated
            // projection (e.g. in a WHERE) — treat as null (3VL).
            Expr::Agg(_) => {
                ExprResult::Failure("aggregate not valid outside a RETURN projection".into())
            }
        }
    }

    /// Runtime evaluation of `EXISTS L` (negated=false) and
    /// `NOT EXISTS L` (negated=true). Two regimes:
    ///
    /// **Uncorrelated** — the body shares no variable with the outer
    /// assignment. The body's truth value is row-independent, so a
    /// single inner run with `limit=1` decides it. Cached as a bool.
    ///
    /// **Correlated** — the body references variables already bound
    /// outside. Evaluated as a semi/anti-join: the body runs once
    /// (no limit), every row is projected onto the correlation
    /// variables and stored in a `HashSet`, and per outer row the
    /// predicate becomes a single O(1) hash probe. Both the row
    /// table and the projection live in the cache, so the body
    /// runs at most once per Runtime regardless of how many outer
    /// rows are evaluated.
    fn eval_exists(&self, mu: &Assignment, body: &Query, negated: bool) -> ExprResult {
        let body_vars = query_freevars(body);
        let outer_keys = mu.keys();
        let mut correlation: Vec<String> = body_vars
            .iter()
            .filter(|v| outer_keys.contains(*v))
            .cloned()
            .collect();
        correlation.sort();

        let body_ptr = body as *const Query as usize;

        if correlation.is_empty() {
            return self.eval_exists_uncorrelated(body, body_ptr, negated);
        }
        self.eval_exists_correlated(mu, body, body_ptr, &correlation, negated)
    }

    fn eval_exists_uncorrelated(&self, body: &Query, body_ptr: usize, negated: bool) -> ExprResult {
        if let Some(ExistsCache::Uncorrelated(b)) = self.exists_cache.borrow().get(&body_ptr) {
            let truth = if negated { !b } else { *b };
            return ExprResult::Success(Value::Bool(truth));
        }
        let ir = self.run_match_chain(body, /*limit=*/ 1);
        let nonempty = !ir.rows.is_empty();
        self.exists_cache
            .borrow_mut()
            .insert(body_ptr, ExistsCache::Uncorrelated(nonempty));
        let truth = if negated { !nonempty } else { nonempty };
        ExprResult::Success(Value::Bool(truth))
    }

    fn eval_exists_correlated(
        &self,
        mu: &Assignment,
        body: &Query,
        body_ptr: usize,
        correlation: &[String],
        negated: bool,
    ) -> ExprResult {
        // Build the cache on first encounter for this body.
        let need_build = !matches!(
            self.exists_cache.borrow().get(&body_ptr),
            Some(ExistsCache::Correlated { .. })
        );
        if need_build {
            let ir = self.run_match_chain(body, /*limit=*/ 0);
            let mut set: HashSet<Vec<PathValue>> = HashSet::with_capacity(ir.rows.len());
            for row in &ir.rows {
                let mut key: Vec<PathValue> = Vec::with_capacity(correlation.len());
                let mut complete = true;
                for v in correlation {
                    match row.assignment.get(v) {
                        Some(pv) => key.push(pv.clone()),
                        None => {
                            // Body row does not bind a correlation
                            // var (e.g. left-joined OPTIONAL row);
                            // it cannot match any outer row on this
                            // key, so skip it.
                            complete = false;
                            break;
                        }
                    }
                }
                if complete {
                    set.insert(key);
                }
            }
            self.exists_cache.borrow_mut().insert(
                body_ptr,
                ExistsCache::Correlated {
                    keys: correlation.to_vec(),
                    set,
                },
            );
        }

        // Probe with the current outer row.
        let cache = self.exists_cache.borrow();
        let (keys, set) = match cache.get(&body_ptr) {
            Some(ExistsCache::Correlated { keys, set }) => (keys, set),
            _ => unreachable!("cache populated above"),
        };
        let mut probe_key: Vec<PathValue> = Vec::with_capacity(keys.len());
        for v in keys {
            match mu.get(v) {
                Some(pv) => probe_key.push(pv.clone()),
                None => {
                    // Should not happen — `correlation` was built
                    // from the intersection with `mu.keys()`. Treat
                    // as no match (drop EXISTS, satisfy NOT EXISTS).
                    return ExprResult::Success(Value::Bool(negated));
                }
            }
        }
        let nonempty = set.contains(&probe_key);
        let truth = if negated { !nonempty } else { nonempty };
        ExprResult::Success(Value::Bool(truth))
    }

    fn eval_binop(op: &BinOp, lv: &Value, rv: &Value) -> ExprResult {
        // Return (a, b, true) if both numeric; a/b as f64 if either operand is Float.
        // Returns None if either is non-numeric.
        fn as_num_pair(lv: &Value, rv: &Value) -> Option<(Value, Value)> {
            match (lv, rv) {
                (Value::Int(_), Value::Int(_)) => Some((lv.clone(), rv.clone())),
                (Value::Float(_), Value::Float(_)) => Some((lv.clone(), rv.clone())),
                (Value::Int(a), Value::Float(_)) => Some((Value::Float(*a as f64), rv.clone())),
                (Value::Float(_), Value::Int(b)) => Some((lv.clone(), Value::Float(*b as f64))),
                _ => None,
            }
        }
        match op {
            BinOp::Add => match as_num_pair(lv, rv) {
                Some((Value::Int(a), Value::Int(b))) => ExprResult::Success(Value::Int(a + b)),
                Some((Value::Float(a), Value::Float(b))) => {
                    ExprResult::Success(Value::Float(a + b))
                }
                _ => ExprResult::Failure("+ requires numeric operands".into()),
            },
            BinOp::Sub => match as_num_pair(lv, rv) {
                Some((Value::Int(a), Value::Int(b))) => ExprResult::Success(Value::Int(a - b)),
                Some((Value::Float(a), Value::Float(b))) => {
                    ExprResult::Success(Value::Float(a - b))
                }
                _ => ExprResult::Failure("- requires numeric operands".into()),
            },
            BinOp::Mul => match as_num_pair(lv, rv) {
                Some((Value::Int(a), Value::Int(b))) => ExprResult::Success(Value::Int(a * b)),
                Some((Value::Float(a), Value::Float(b))) => {
                    ExprResult::Success(Value::Float(a * b))
                }
                _ => ExprResult::Failure("* requires numeric operands".into()),
            },
            BinOp::Gt => match as_num_pair(lv, rv) {
                Some((Value::Int(a), Value::Int(b))) => ExprResult::Success(Value::Bool(a > b)),
                Some((Value::Float(a), Value::Float(b))) => ExprResult::Success(Value::Bool(a > b)),
                _ => ExprResult::Failure("> requires numeric operands".into()),
            },
            BinOp::Lt => match as_num_pair(lv, rv) {
                Some((Value::Int(a), Value::Int(b))) => ExprResult::Success(Value::Bool(a < b)),
                Some((Value::Float(a), Value::Float(b))) => ExprResult::Success(Value::Bool(a < b)),
                _ => ExprResult::Failure("< requires numeric operands".into()),
            },
            BinOp::Ge => match as_num_pair(lv, rv) {
                Some((Value::Int(a), Value::Int(b))) => ExprResult::Success(Value::Bool(a >= b)),
                Some((Value::Float(a), Value::Float(b))) => {
                    ExprResult::Success(Value::Bool(a >= b))
                }
                _ => ExprResult::Failure(">= requires numeric operands".into()),
            },
            BinOp::Le => match as_num_pair(lv, rv) {
                Some((Value::Int(a), Value::Int(b))) => ExprResult::Success(Value::Bool(a <= b)),
                Some((Value::Float(a), Value::Float(b))) => {
                    ExprResult::Success(Value::Bool(a <= b))
                }
                _ => ExprResult::Failure("<= requires numeric operands".into()),
            },
            BinOp::Eq => ExprResult::Success(Value::Bool(lv == rv)),
            BinOp::Ne => ExprResult::Success(Value::Bool(lv != rv)),
            BinOp::In => match rv {
                Value::List(items) => {
                    ExprResult::Success(Value::Bool(items.iter().any(|x| x == lv)))
                }
                _ => ExprResult::Failure("'in' requires a list on the right".into()),
            },
            BinOp::And => match (lv, rv) {
                (Value::Bool(a), Value::Bool(b)) => ExprResult::Success(Value::Bool(*a && *b)),
                _ => ExprResult::Failure("and requires bools".into()),
            },
            BinOp::Or => match (lv, rv) {
                (Value::Bool(a), Value::Bool(b)) => ExprResult::Success(Value::Bool(*a || *b)),
                _ => ExprResult::Failure("or requires bools".into()),
            },
            _ => ExprResult::Failure(format!("unexpected op {op} in eval_binop")),
        }
    }
}

/// Natural join of two intermediate result sets on shared assignment keys
/// Collect every variable name introduced anywhere inside the query
/// body (across all match clauses). Used by the existential evaluator
/// to decide whether a body is correlated with the surrounding scope:
/// the intersection with the outer `Assignment` keys gives the
/// correlation set.
fn query_freevars(q: &Query) -> HashSet<String> {
    let mut acc = HashSet::new();
    for m in &q.matches {
        acc.extend(m.pattern().freevars());
    }
    acc
}

/// (variables that appear on both sides). When no key is shared, the result
/// is the cross-product. Used by the per-match evaluator only when at least
/// one match is OPTIONAL — all-Simple queries still go through the LTJ /
/// hash-join path inside `run_path_pattern`.
fn natural_join(
    left: &IntermediateResult,
    right: &IntermediateResult,
    limit: usize,
) -> IntermediateResult {
    let shared: Vec<String> = {
        let lk: HashSet<String> = left
            .rows
            .first()
            .map(|r| r.assignment.keys())
            .unwrap_or_default();
        let rk: HashSet<String> = right
            .rows
            .first()
            .map(|r| r.assignment.keys())
            .unwrap_or_default();
        lk.intersection(&rk).cloned().collect()
    };

    let join_var = shared.first();
    let mut rows = Vec::new();

    if let Some(jv) = join_var {
        let mut by_val: HashMap<&PathValue, Vec<usize>> = HashMap::new();
        for (i, r) in right.rows.iter().enumerate() {
            if let Some(pv) = r.assignment.get(jv) {
                by_val.entry(pv).or_default().push(i);
            }
        }
        'outer: for r1 in &left.rows {
            let Some(pv) = r1.assignment.get(jv) else {
                continue;
            };
            let Some(idxs) = by_val.get(pv) else {
                continue;
            };
            for &idx in idxs {
                let r2 = &right.rows[idx];
                if r1.assignment.can_unify(&r2.assignment) {
                    rows.push(ResultRow::join(r1, r2, r1.assignment.unify(&r2.assignment)));
                    if limit > 0 && rows.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
    } else {
        'outer2: for r1 in &left.rows {
            for r2 in &right.rows {
                rows.push(ResultRow::join(r1, r2, r1.assignment.unify(&r2.assignment)));
                if limit > 0 && rows.len() >= limit {
                    break 'outer2;
                }
            }
        }
    }
    IntermediateResult::new(rows)
}

/// Left outer join — runtime counterpart of `TypeEnvironment::outer_join`.
///
/// For each row in `left`, find unifying rows in `right` (same predicate as
/// natural join). If at least one matches, emit all unified rows (success
/// branch). If none match, emit the left row alone with every variable in
/// `new_vars \ bound_vars` set to `PathValue::Nothing` (unsuccess branch).
/// `Nothing` flows into projection as `Value::Null` because `pv.id()`
/// returns `None`, which `AttrLookup` turns into `Failure`, which
/// `eval_expr_item` maps to `Value::Null`.
fn left_outer_join(
    left: &IntermediateResult,
    right: &IntermediateResult,
    bound_vars: &HashSet<String>,
    new_vars: &HashSet<String>,
) -> IntermediateResult {
    let pad_vars: Vec<String> = new_vars.difference(bound_vars).cloned().collect();
    let mut rows = Vec::new();

    for r1 in &left.rows {
        let mut matched_any = false;
        for r2 in &right.rows {
            if r1.assignment.can_unify(&r2.assignment) {
                matched_any = true;
                rows.push(ResultRow::join(r1, r2, r1.assignment.unify(&r2.assignment)));
            }
        }
        if !matched_any {
            let mut padded = r1.assignment.clone();
            for v in &pad_vars {
                padded.extend(v.clone(), PathValue::Nothing);
            }
            rows.push(ResultRow::with_paths(r1.paths.clone(), padded));
        }
    }
    IntermediateResult::new(rows)
}

// Aggregate reducers (ISO §20.9 GR 7a-iii..vi). Inputs already passed
// null-elimination and optional DISTINCT in collect_aggregate_values.
// Empty input → `Value::Null`.

fn null_value() -> Value {
    Value::Null
}

/// Int-preserving when all inputs are Int; promotes to Float on any
/// Float input. Non-numeric skipped (gradual tolerance). Empty → null.
fn sum_values(values: &[Value]) -> Value {
    let mut int_acc: i64 = 0;
    let mut float_acc: f64 = 0.0;
    let mut had_float = false;
    let mut had_value = false;
    for v in values {
        match v {
            Value::Int(n) => {
                if had_float {
                    float_acc += *n as f64;
                } else {
                    int_acc = int_acc.wrapping_add(*n);
                }
                had_value = true;
            }
            Value::Float(f) => {
                if !had_float {
                    float_acc = int_acc as f64;
                    had_float = true;
                }
                float_acc += f;
                had_value = true;
            }
            _ => {} // skip non-numeric
        }
    }
    if !had_value {
        null_value()
    } else if had_float {
        Value::Float(float_acc)
    } else {
        Value::Int(int_acc)
    }
}

/// Always Float (averages of ints usually aren't ints). Empty → null.
/// Convert a `PathValue` (binding-table value) to a `Value` (projected
/// row value). Repetition `Group(...)` becomes `Value::List(...)` so a
/// `RETURN y` after `MATCH (x)-[y]->{1,n}()` lands as a list of edge
/// references instead of collapsing to NULL.
fn path_value_to_value(pv: &PathValue) -> Value {
    match pv {
        PathValue::Node(id) => Value::Node(*id),
        PathValue::EdgeDirectional(id) | PathValue::EdgeUndirectional(id) => Value::Edge(*id),
        PathValue::Nothing => Value::Null,
        PathValue::Group(items) => Value::List(items.iter().map(path_value_to_value).collect()),
    }
}

fn avg_values(values: &[Value]) -> Value {
    let mut sum: f64 = 0.0;
    let mut count: u64 = 0;
    for v in values {
        let n = match v {
            Value::Int(n) => *n as f64,
            Value::Float(f) => *f,
            _ => continue, // skip non-numeric
        };
        sum += n;
        count += 1;
    }
    if count == 0 {
        null_value()
    } else {
        Value::Float(sum / count as f64)
    }
}

/// Smallest by `value_cmp`. Incomparable with running best → skipped.
fn min_values(values: &[Value]) -> Value {
    let mut best: Option<&Value> = None;
    for v in values {
        match best {
            None => best = Some(v),
            Some(current) => {
                if let Some(std::cmp::Ordering::Less) = value_cmp(v, current) {
                    best = Some(v);
                }
            }
        }
    }
    best.cloned().unwrap_or_else(null_value)
}

fn max_values(values: &[Value]) -> Value {
    let mut best: Option<&Value> = None;
    for v in values {
        match best {
            None => best = Some(v),
            Some(current) => {
                if let Some(std::cmp::Ordering::Greater) = value_cmp(v, current) {
                    best = Some(v);
                }
            }
        }
    }
    best.cloned().unwrap_or_else(null_value)
}

/// O(n) dedup preserving insertion order via HashSet membership.
fn dedup_preserving_order(rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    let mut seen: HashSet<GroupKey> = HashSet::with_capacity(rows.len());
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
    for row in rows {
        let key = GroupKey::from_values(row.clone());
        if seen.insert(key) {
            out.push(row);
        }
    }
    out
}

// `Value` lacks Hash/Eq because of `f64`. This wrapper is runtime-local;
// modifying `Value` would touch ~30 call sites. Floats are normalized so
// NaN==NaN and +0.0==-0.0 (consistent with our notion of "same group").

#[derive(Debug, Clone)]
struct GroupKey(Vec<Value>);

fn hash_value<H: Hasher>(v: &Value, state: &mut H) {
    std::mem::discriminant(v).hash(state);
    match v {
        Value::Null => {} // discriminant alone identifies the variant
        Value::Int(n) => n.hash(state),
        Value::Float(f) => normalize_float_bits(*f).hash(state),
        Value::Str(s) => s.hash(state),
        Value::Bool(b) => b.hash(state),
        Value::List(items) => {
            items.len().hash(state);
            for item in items {
                hash_value(item, state);
            }
        }
        Value::Record(fields) => {
            fields.len().hash(state);
            for (k, v) in fields {
                k.hash(state);
                hash_value(v, state);
            }
        }
        Value::Node(id) | Value::Edge(id) => id.hash(state),
    }
}

fn eq_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => normalize_float_bits(*x) == normalize_float_bits(*y),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| eq_value(a, b))
        }
        (Value::Record(x), Value::Record(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, vx)| y.get(k).is_some_and(|vy| eq_value(vx, vy)))
        }
        _ => false,
    }
}

/// NaN → canonical NaN bits; -0.0 → +0.0 bits. Keeps Hash/Eq consistent
/// with our notion that NaN==NaN and +0.0==-0.0 for grouping.
fn normalize_float_bits(f: f64) -> u64 {
    if f.is_nan() {
        f64::NAN.to_bits()
    } else if f == 0.0 {
        0
    } else {
        f.to_bits()
    }
}

impl PartialEq for GroupKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .zip(other.0.iter())
                .all(|(a, b)| eq_value(a, b))
    }
}

impl Eq for GroupKey {}

impl Hash for GroupKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for v in &self.0 {
            hash_value(v, state);
        }
    }
}

impl GroupKey {
    fn from_values(vs: Vec<Value>) -> Self {
        GroupKey(vs)
    }
}

/// Post-projection sort over already-projected rows. Caller
/// guarantees every spec is a `SortKey::Column` (typechecker rejects
/// the mixed case). `limit > 0` enables the top-k heap path.
fn sort_projected_rows(rows: &mut Vec<Vec<Value>>, specs: &[SortSpec], limit: usize) {
    let mut decorated: Vec<(Vec<Option<Value>>, Vec<Value>)> = std::mem::take(rows)
        .into_iter()
        .map(|projected| {
            let keys: Vec<Option<Value>> = specs
                .iter()
                .map(|s| match &s.key {
                    SortKey::Column(idx) => match projected.get(*idx) {
                        Some(Value::Null) | None => None,
                        Some(v) => Some(v.clone()),
                    },
                    SortKey::Expr(_) => unreachable!(
                        "Expr sort key reached post-projection path — typechecker \
                         should have rejected mixed Expr/Column"
                    ),
                })
                .collect();
            (keys, projected)
        })
        .collect();
    sort_decorated(&mut decorated, specs, limit);
    rows.extend(decorated.into_iter().map(|(_, p)| p));
}

/// True when the pattern carries at least one edge of any direction.
/// LTJ requires triples and a triple requires an edge — without one,
/// `try_ltj` returns None and the BTree-LTJ-real path can't pin the
/// sort variable through the index-fold pipeline.
fn pattern_has_edge(p: &PathPattern) -> bool {
    match p {
        PathPattern::EdgeRight(_)
        | PathPattern::EdgeLeft(_)
        | PathPattern::EdgeUndirected(_)
        | PathPattern::EdgeAnyDirection(_) => true,
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            pattern_has_edge(a) || pattern_has_edge(b)
        }
        PathPattern::Filter(p, _)
        | PathPattern::Repeat { pattern: p, .. }
        | PathPattern::Questioned(p)
        | PathPattern::Selected { pattern: p, .. } => pattern_has_edge(p),
        PathPattern::Node(_) => false,
    }
}

/// One cursor in the btree-LTJ-real merge walk. Holds the per-label
/// sorted id stream from `lookup_node_ordered` plus the cached
/// primary-attr value of the head id. The merge driver advances the
/// cursor by stepping `pos` and re-reading `head_value` from the
/// graph; reads come from the page cache in the LDBC-class workloads
/// that motivate the merge.
///
/// Single-label paths use one cursor — the merge degenerates to the
/// original linear walk over the single btree's id list.
#[derive(Debug)]
struct BtreeMergeCursor {
    /// Label associated with this btree. Diagnostic only — the merge
    /// driver never has to disambiguate by label.
    _label: String,
    ids: Vec<Id>,
    pos: usize,
    head_value: Option<Value>,
}

impl BtreeMergeCursor {
    fn new<G: GraphAccess>(graph: &G, label: String, ids: Vec<Id>, attr: &str) -> Self {
        let head_value = ids
            .first()
            .and_then(|&id| graph.node_props(id).get(attr).cloned());
        Self {
            _label: label,
            ids,
            pos: 0,
            head_value,
        }
    }

    fn current_id(&self) -> Option<Id> {
        self.ids.get(self.pos).copied()
    }

    fn advance<G: GraphAccess>(&mut self, graph: &G, attr: &str) {
        self.pos += 1;
        self.head_value = self
            .ids
            .get(self.pos)
            .and_then(|&id| graph.node_props(id).get(attr).cloned());
    }

    fn is_done(&self) -> bool {
        self.pos >= self.ids.len()
    }
}

/// Pick the cursor whose `head_value` is best (smallest if `asc`,
/// largest otherwise). Returns the cursor index, or `None` when every
/// cursor is exhausted. Linear scan over the cursors — for the
/// expected `Or`-arity (2-3 labels) it beats a heap and avoids the
/// allocation. Cursors with `Some(value)` always rank ahead of those
/// with `None` (a missing primary attr): the latter still need to be
/// tried by the LTJ but only after every value-bearing id has been
/// processed, which the cohort-detection break never reaches in
/// practice on indexed workloads.
fn pick_best_cursor(cursors: &[BtreeMergeCursor], asc: bool) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, c) in cursors.iter().enumerate() {
        if c.is_done() {
            continue;
        }
        let Some(b) = best else {
            best = Some(i);
            continue;
        };
        // Compare via the runtime's `cmp_values`, which knows the
        // GQL ordering for `Int`/`Float`/`Str`/`Bool` (the only types
        // a btree indexes anyway). A `None` head ranks behind any
        // `Some` head — the LTJ may still want to try the id but
        // only after the value-bearing cursors are drained, which the
        // cohort early-exit never reaches in practice.
        let take_c = match (&cursors[b].head_value, &c.head_value) {
            (Some(_), None) => false,
            (None, Some(_)) => true,
            (None, None) => false,
            (Some(bv), Some(cv)) => {
                if asc {
                    cmp_values(cv, BinOp::Lt, bv)
                } else {
                    cmp_values(cv, BinOp::Gt, bv)
                }
            }
        };
        if take_c {
            best = Some(i);
        }
    }
    best
}

/// Walk every `Descriptor` in `matches` and collect the required
/// labels declared on the descriptor whose `var` matches `target`.
/// Required labels are the conjunctive positive leaves
/// (`required_labels()` on `LabelType`); patterns under `Or` / `Neg`
/// don't constrain the variable enough to feed a btree lookup.
fn labels_for_var(matches: &[MatchStatement], target: &str) -> Vec<String> {
    fn walk(p: &PathPattern, target: &str, out: &mut Vec<String>) {
        match p {
            PathPattern::Node(Some(d))
            | PathPattern::EdgeRight(Some(d))
            | PathPattern::EdgeLeft(Some(d))
            | PathPattern::EdgeUndirected(Some(d))
            | PathPattern::EdgeAnyDirection(Some(d))
                if d.var.as_deref() == Some(target) =>
            {
                for l in d.dtype.label.required_labels() {
                    out.push(l.to_string());
                }
            }
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                walk(a, target, out);
                walk(b, target, out);
            }
            PathPattern::Filter(p, _)
            | PathPattern::Repeat { pattern: p, .. }
            | PathPattern::Questioned(p) => walk(p, target, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for m in matches {
        walk(m.pattern(), target, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Or-candidate labels for `target`, when its descriptor's label type
/// is a pure disjunction of bare labels (`A | B | …`). Returns `None`
/// for any other shape (And, Neg, Star, mixed). Collected across every
/// descriptor declaration of `target` in the chain; if any declaration
/// is non-Or-of-leaves, the whole result is `None` because we can't
/// safely under-cover the merge from one and over-cover from another.
///
/// Used by the btree-LTJ-real multi-label merge path; the existing
/// `labels_for_var` (required-labels) handles And / bare cases via a
/// single-btree pick.
fn or_candidate_labels_for_var(matches: &[MatchStatement], target: &str) -> Option<Vec<String>> {
    fn walk(p: &PathPattern, target: &str, out: &mut Option<Vec<String>>) {
        match p {
            PathPattern::Node(Some(d))
            | PathPattern::EdgeRight(Some(d))
            | PathPattern::EdgeLeft(Some(d))
            | PathPattern::EdgeUndirected(Some(d))
            | PathPattern::EdgeAnyDirection(Some(d))
                if d.var.as_deref() == Some(target) =>
            {
                if out.is_none() {
                    return;
                }
                let Some(cands) = d.dtype.label.or_candidate_labels() else {
                    *out = None;
                    return;
                };
                if let Some(acc) = out.as_mut() {
                    for l in cands {
                        acc.push(l.to_string());
                    }
                }
            }
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                walk(a, target, out);
                walk(b, target, out);
            }
            PathPattern::Filter(p, _)
            | PathPattern::Repeat { pattern: p, .. }
            | PathPattern::Questioned(p) => walk(p, target, out),
            _ => {}
        }
    }
    let mut out: Option<Vec<String>> = Some(Vec::new());
    for m in matches {
        walk(m.pattern(), target, &mut out);
    }
    let mut v = out?;
    if v.is_empty() {
        return None;
    }
    v.sort();
    v.dedup();
    Some(v)
}

/// Build a btree-ordered output by bucketing rows on `mu[var_name].id()`
/// and walking `ordered_ids` in sort order. Rows whose binding for
/// `var_name` is missing or not a Node go to the "nulls" bucket and
/// are placed before / after the indexed run per `nulls_first`.
/// `limit == 0` means no truncation.
fn btree_bucket_output(
    rows: Vec<ResultRow>,
    var_name: &str,
    ordered_ids: &[Id],
    limit: usize,
    nulls_first: bool,
) -> Vec<ResultRow> {
    let total = rows.len();
    let mut taken: Vec<Option<ResultRow>> = rows.into_iter().map(Some).collect();
    let mut by_id: HashMap<Id, Vec<usize>> = HashMap::new();
    let mut nulls: Vec<usize> = Vec::new();
    for (i, slot) in taken.iter().enumerate() {
        let row = slot.as_ref().expect("just-built buffer is fully populated");
        match row.assignment.get(var_name) {
            Some(PathValue::Node(id)) => by_id.entry(*id).or_default().push(i),
            _ => nulls.push(i),
        }
    }

    let cap = if limit == 0 { total } else { limit.min(total) };
    let mut out: Vec<ResultRow> = Vec::with_capacity(cap);

    let push_indexed = |out: &mut Vec<ResultRow>,
                        taken: &mut [Option<ResultRow>],
                        by_id: &mut HashMap<Id, Vec<usize>>|
     -> bool {
        for &id in ordered_ids {
            let Some(idxs) = by_id.remove(&id) else {
                continue;
            };
            for i in idxs {
                if let Some(r) = taken[i].take() {
                    out.push(r);
                    if out.len() >= cap {
                        return true;
                    }
                }
            }
        }
        false
    };
    let push_nulls = |out: &mut Vec<ResultRow>, taken: &mut [Option<ResultRow>]| -> bool {
        for &i in &nulls {
            if let Some(r) = taken[i].take() {
                out.push(r);
                if out.len() >= cap {
                    return true;
                }
            }
        }
        false
    };

    if nulls_first {
        if push_nulls(&mut out, &mut taken) {
            return out;
        }
        push_indexed(&mut out, &mut taken, &mut by_id);
    } else if !push_indexed(&mut out, &mut taken, &mut by_id) {
        push_nulls(&mut out, &mut taken);
    }
    out
}

/// Sort a decorated `[(keys, payload)]` buffer in-place. Routes to one
/// of three implementations:
///
/// - `pdqsort` — Rust's `sort_unstable_by`, O(n log n), in place. The
///   default for the full-sort path. (Since Rust 1.81 the unstable sort
///   is `ipnsort`; the name is kept for the env flag and history.)
/// - `topk` — bounded max-heap of size k, O(n log k). Used when the
///   query carries a usable `LIMIT k` and `k < n / 2`. The heap holds
///   the k entries that come earliest in output order; entries that
///   exceed the running max are dropped on sight.
/// - `driftsort` — Rust's stable `sort_by` (driftsort since 1.81: an
///   adaptive merge sort with the Powersort merge policy). Does fewer
///   comparisons than pdqsort and runs near O(n) on pre-sorted input,
///   at the cost of O(n) scratch. Benchmark-only via the force flag;
///   never auto-selected, since GQL leaves peer order
///   implementation-dependent (§16.17 GR 1k/US006) so stability buys
///   nothing the default path needs.
///
/// Override the heuristic with `GQLITE_ORDERBY_FORCE=pdqsort|topk|driftsort` —
/// useful for benchmarking the worst case of each algorithm in
/// isolation. Force=topk silently degrades to pdqsort when `limit == 0`
/// or `limit >= n` (no top-k to extract).
fn sort_decorated<T>(
    decorated: &mut Vec<(Vec<Option<Value>>, T)>,
    specs: &[SortSpec],
    limit: usize,
) {
    let n = decorated.len();
    let force = std::env::var("GQLITE_ORDERBY_FORCE").ok();
    let topk_applies = limit > 0 && limit < n;
    let use_topk = match force.as_deref() {
        Some("topk") => topk_applies,
        Some("pdqsort") | Some("driftsort") => false,
        _ => topk_applies && limit * 2 < n,
    };
    if use_topk {
        let owned = std::mem::take(decorated);
        *decorated = select_topk_decorated(owned, specs, limit);
    } else if matches!(force.as_deref(), Some("driftsort")) {
        decorated.sort_by(|(a, _), (b, _)| compare_sort_keys(a, b, specs));
    } else {
        decorated.sort_unstable_by(|(a, _), (b, _)| compare_sort_keys(a, b, specs));
    }
}

/// Hand-rolled max-heap of size `k` keyed by `compare_sort_keys`. Returns
/// the k earliest items in output order. We avoid `std::collections::
/// BinaryHeap` because its `Ord`-bound API doesn't let us thread a
/// borrowed `specs` slice into comparisons without per-entry cloning.
fn select_topk_decorated<T>(
    items: Vec<(Vec<Option<Value>>, T)>,
    specs: &[SortSpec],
    k: usize,
) -> Vec<(Vec<Option<Value>>, T)> {
    if k == 0 {
        return Vec::new();
    }
    let mut heap: Vec<(Vec<Option<Value>>, T)> = Vec::with_capacity(k);
    for (keys, row) in items {
        if heap.len() < k {
            heap.push((keys, row));
            if heap.len() == k {
                for i in (0..k / 2).rev() {
                    sift_down(&mut heap, i, specs);
                }
            }
        } else if compare_sort_keys(&keys, &heap[0].0, specs).is_lt() {
            heap[0] = (keys, row);
            sift_down(&mut heap, 0, specs);
        }
    }
    heap.sort_unstable_by(|(a, _), (b, _)| compare_sort_keys(a, b, specs));
    heap
}

fn sift_down<T>(heap: &mut [(Vec<Option<Value>>, T)], mut i: usize, specs: &[SortSpec]) {
    let n = heap.len();
    loop {
        let l = 2 * i + 1;
        let r = 2 * i + 2;
        let mut largest = i;
        if l < n && compare_sort_keys(&heap[l].0, &heap[largest].0, specs).is_gt() {
            largest = l;
        }
        if r < n && compare_sort_keys(&heap[r].0, &heap[largest].0, specs).is_gt() {
            largest = r;
        }
        if largest == i {
            break;
        }
        heap.swap(i, largest);
        i = largest;
    }
}

/// ISO §16.17 GR 1g comparator over pre-computed key tuples. `None`
/// is the null value; default null ordering is NULLS LAST per SR 6.
/// Cross-kind values fall back to Equal per US007.
fn compare_sort_keys(
    a: &[Option<Value>],
    b: &[Option<Value>],
    specs: &[SortSpec],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    debug_assert_eq!(a.len(), specs.len());
    debug_assert_eq!(b.len(), specs.len());

    for (i, spec) in specs.iter().enumerate() {
        let nulls = spec.nulls.unwrap_or(NullsOrder::Last);
        let ord = match (&a[i], &b[i]) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => match nulls {
                NullsOrder::First => Ordering::Less,
                NullsOrder::Last => Ordering::Greater,
            },
            (Some(_), None) => match nulls {
                NullsOrder::First => Ordering::Greater,
                NullsOrder::Last => Ordering::Less,
            },
            (Some(av), Some(bv)) => match value_cmp(av, bv) {
                Some(o) => match spec.dir {
                    SortDir::Asc => o,
                    SortDir::Desc => o.reverse(),
                },
                None => Ordering::Equal,
            },
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// MIN/MAX ordering. Int<->Float promote; cross-kind returns None.
fn value_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(match (x, y) {
            (false, true) => Ordering::Less,
            (true, false) => Ordering::Greater,
            _ => Ordering::Equal,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod group_key_tests {
    //! Tests for `GroupKey` — the Hash + Eq wrapper used for grouping
    //! and dedup. The key invariant: two GroupKeys that compare equal
    //! must hash equal (Rust's Hash/Eq contract). Float normalization
    //! is what makes this hold for NaN and -0.0.
    use super::*;
    use std::collections::HashMap;

    fn key(vs: Vec<Value>) -> GroupKey {
        GroupKey::from_values(vs)
    }

    fn hash_of(k: &GroupKey) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut h = DefaultHasher::new();
        k.hash(&mut h);
        h.finish()
    }

    #[test]
    fn equal_ints_are_equal_and_hash_same() {
        let a = key(vec![Value::Int(42), Value::Str("x".into())]);
        let b = key(vec![Value::Int(42), Value::Str("x".into())]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn distinct_ints_are_unequal() {
        let a = key(vec![Value::Int(1)]);
        let b = key(vec![Value::Int(2)]);
        assert_ne!(a, b);
    }

    #[test]
    fn nan_equals_nan_under_group_key() {
        // IEEE 754 says NaN != NaN. For grouping we want them collapsed
        // into the same group, so GroupKey treats NaN as self-equal.
        let a = key(vec![Value::Float(f64::NAN)]);
        let b = key(vec![Value::Float(f64::NAN)]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn nan_payloads_are_normalized() {
        // Two NaNs constructed with different bit patterns still group
        // together — `normalize_float_bits` collapses them.
        let nan1 = f64::from_bits(0x7ff8_0000_0000_0001);
        let nan2 = f64::from_bits(0x7ff8_0000_0000_0002);
        assert!(nan1.is_nan() && nan2.is_nan());
        assert_ne!(nan1.to_bits(), nan2.to_bits()); // sanity: bit-distinct
        let a = key(vec![Value::Float(nan1)]);
        let b = key(vec![Value::Float(nan2)]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn positive_and_negative_zero_are_equal() {
        // IEEE: +0.0 == -0.0, but their bits differ. GroupKey must
        // treat them as the same group to stay consistent with `==`.
        let a = key(vec![Value::Float(0.0)]);
        let b = key(vec![Value::Float(-0.0)]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn distinct_floats_are_unequal() {
        let a = key(vec![Value::Float(1.5)]);
        let b = key(vec![Value::Float(1.6)]);
        assert_ne!(a, b);
    }

    #[test]
    fn nested_list_equality_is_structural() {
        let a = key(vec![Value::List(vec![Value::Int(1), Value::Int(2)])]);
        let b = key(vec![Value::List(vec![Value::Int(1), Value::Int(2)])]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn nested_record_equality_is_structural() {
        let mut r1 = std::collections::BTreeMap::new();
        r1.insert("k".into(), Value::Int(1));
        let mut r2 = std::collections::BTreeMap::new();
        r2.insert("k".into(), Value::Int(1));
        let a = key(vec![Value::Record(r1)]);
        let b = key(vec![Value::Record(r2)]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn cross_kind_values_are_unequal() {
        // Same numeric value, different kinds — not equal.
        let a = key(vec![Value::Int(1)]);
        let b = key(vec![Value::Float(1.0)]);
        assert_ne!(a, b);
    }

    #[test]
    fn works_as_hashmap_key() {
        // End-to-end smoke test of the actual use case.
        let mut m: HashMap<GroupKey, usize> = HashMap::new();
        m.insert(key(vec![Value::Int(1)]), 0);
        m.insert(key(vec![Value::Int(2)]), 1);
        assert_eq!(m.get(&key(vec![Value::Int(1)])), Some(&0));
        assert_eq!(m.get(&key(vec![Value::Int(2)])), Some(&1));
        assert_eq!(m.get(&key(vec![Value::Int(3)])), None);
    }
}
