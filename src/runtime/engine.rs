use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Path, PathValue, Value};
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{Aggregator, GeneralSetKind, Query, ReturnItem, SetQuantifier};
use crate::typing::descriptor_type::DescriptorType;
use crate::typing::label_type::LabelType;
use crate::typing::property_type::PropertyType;
use crate::typing::simple_type::SimpleType;

use super::assignment::Assignment;
use super::ltj::pattern_extract;
use super::ltj::triple_index::TripleIndex;
use super::result::{ExprResult, IntermediateResult, QueryResult, ResultRow};

/// Runtime engine for evaluating GQL path patterns on a graph.
/// Generic over `GraphAccess` — works with both in-memory Graph and file-backed GraphStore.
///
/// Optimizations:
/// - Label-indexed scanning: uses label index when descriptor has a simple label
/// - Adjacency-driven concat: uses adjacency lists when right side is an edge/node pattern
pub struct Runtime<'g, G: GraphAccess> {
    pub graph: &'g G,
}

impl<'g, G: GraphAccess> Runtime<'g, G> {
    pub fn new(graph: &'g G) -> Self {
        Self { graph }
    }

    pub fn run(&self, pattern: &PathPattern) -> IntermediateResult {
        self.run_path_pattern(pattern, 0)
    }

    /// Run with a result limit (0 = unlimited). Stops early once limit is reached.
    pub fn run_with_limit(&self, pattern: &PathPattern, limit: usize) -> IntermediateResult {
        self.run_path_pattern(pattern, limit)
    }

    /// Run a full Query (MATCH ... WHERE ... RETURN).
    /// Returns projected rows as Vec<Vec<Value>> if RETURN is specified,
    /// or the raw IntermediateResult if no RETURN clause.
    ///
    /// Two projection paths:
    /// - **Row-by-row** (no aggregates in RETURN): each input row produces
    ///   one output row, optionally deduplicated by `DISTINCT`. The
    ///   `limit` cuts input rows directly (early termination).
    /// - **Group-and-aggregate** (any aggregate in RETURN): rows are grouped
    ///   by the values of the non-aggregate items, and each aggregate is
    ///   reduced over its group. The `limit` cuts the OUTPUT row count
    ///   (number of groups), not the input — truncating input would
    ///   corrupt aggregate values (e.g. COUNT(*) would only count the
    ///   first `limit` matched rows). `RETURN DISTINCT` paired with
    ///   aggregates dedupes the projected output rows; with implicit
    ///   GROUP BY this is rarely observable (group keys are already
    ///   unique) but the SQL-style semantics are preserved.
    pub fn run_query(&self, query: &Query, limit: usize) -> QueryResult {
        let return_items = match &query.returns {
            None => {
                let ir = self.run_path_pattern(&query.pattern, limit);
                return QueryResult::Raw(ir);
            }
            Some(items) => items,
        };

        let has_aggs = return_items.iter().any(|i| i.is_aggregate());

        // Aggregates must see every matched row, so the input scan runs
        // unlimited and `limit` is applied to the post-aggregation output.
        let input_limit = if has_aggs { 0 } else { limit };
        let ir = self.run_path_pattern(&query.pattern, input_limit);

        let projected = if has_aggs {
            let mut p = self.run_aggregated(return_items, query.group_by.as_deref(), &ir.rows);
            if query.distinct {
                p = dedup_preserving_order(p);
            }
            if limit > 0 && p.len() > limit {
                p.truncate(limit);
            }
            p
        } else {
            self.run_row_by_row(return_items, &ir.rows, query.distinct)
        };

        QueryResult::Projected(projected)
    }

    /// Row-by-row projection: one output row per input row. The behavior
    /// is unchanged from before aggregates landed.
    fn run_row_by_row(
        &self,
        items: &[ReturnItem],
        rows: &[ResultRow],
        distinct: bool,
    ) -> Vec<Vec<Value>> {
        let mut projected: Vec<Vec<Value>> = Vec::new();
        for row in rows {
            let vals: Vec<Value> = items
                .iter()
                .map(|item| self.eval_expr_item(item, &row.assignment))
                .collect();
            if distinct {
                if !projected.contains(&vals) {
                    projected.push(vals);
                }
            } else {
                projected.push(vals);
            }
        }
        projected
    }

    /// Evaluate an `Expr`-shaped return item against one row's assignment.
    /// Aggregates can't be projected per-row — calling this on an aggregate
    /// is a runtime bug (caller forgot to take the aggregated path).
    fn eval_expr_item(&self, item: &ReturnItem, mu: &Assignment) -> Value {
        match item {
            ReturnItem::Expr { expr, .. } => match self.run_expr(mu, expr) {
                ExprResult::Success(v) => v,
                ExprResult::Failure(_) => Value::Str("NULL".into()),
            },
            ReturnItem::Aggregate { .. } => {
                unreachable!("aggregate items must be projected via run_aggregated")
            }
        }
    }

    /// Group-and-aggregate projection.
    ///
    /// The grouping keys come from one of two sources:
    /// - **Explicit GROUP BY** (`explicit_group_by = Some(exprs)`, ISO
    ///   §16.15 / Feature GQ15): the listed expressions are evaluated
    ///   per row to build the key. RETURN may project any aggregate plus
    ///   any non-aggregate Expr that is structurally equal to a group-by
    ///   expression.
    /// - **Implicit (Cypher-style)** (`explicit_group_by = None`): every
    ///   non-aggregate item in RETURN forms part of the key. Convenient
    ///   but a divergence from strict ISO; used as the default fallback.
    ///
    /// Either way:
    /// 1. Rows are partitioned by their key tuple.
    /// 2. Each group emits one output row: aggregate items are reduced
    ///    over the group's row set; non-aggregate items resolve from the
    ///    group's first row (they're invariant within the group).
    ///
    /// Edge case (ISO §20.9 GR 7a-i): a query with no key items and zero
    /// input rows still emits one output row — `RETURN COUNT(*)` over an
    /// empty match yields `[[0]]`, not `[]`.
    ///
    /// Performance: O(n) total. `Value` lacks `Hash`/`Ord` (because of
    /// `f64`), so we use a private `GroupKey` wrapper with manual
    /// `Hash + Eq` (NaN normalized, +0/-0 collapsed) as the HashMap key.
    /// Insertion order is preserved by tracking groups in a parallel Vec.
    fn run_aggregated(
        &self,
        items: &[ReturnItem],
        explicit_group_by: Option<&[Expr]>,
        rows: &[ResultRow],
    ) -> Vec<Vec<Value>> {
        // group_indices[i] = row indices for the i-th group (insertion order
        // preserved so output is deterministic). key_to_index gives O(1)
        // lookup of "which group has this key?" using GroupKey's Hash impl.
        let mut group_indices: Vec<Vec<usize>> = Vec::new();
        let mut key_to_index: HashMap<GroupKey, usize> = HashMap::new();

        // Compute key for each row. Source depends on explicit vs implicit.
        for (row_idx, row) in rows.iter().enumerate() {
            let key_values: Vec<Value> = match explicit_group_by {
                Some(exprs) => exprs
                    .iter()
                    .map(|e| match self.run_expr(&row.assignment, e) {
                        ExprResult::Success(v) => v,
                        ExprResult::Failure(_) => Value::Str("NULL".into()),
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

        // Pure-aggregate edge case: zero rows + no group key → emit one
        // empty-group output row (so `COUNT(*)` over no matches = 0).
        let key_arity = explicit_group_by
            .map(|e| e.len())
            .unwrap_or_else(|| items.iter().filter(|it| !it.is_aggregate()).count());
        if key_arity == 0 && group_indices.is_empty() {
            group_indices.push(Vec::new());
        }

        // Project each group to one output row.
        let mut out: Vec<Vec<Value>> = Vec::with_capacity(group_indices.len());
        for row_idxs in &group_indices {
            let row_vals: Vec<Value> = items
                .iter()
                .map(|item| match item {
                    ReturnItem::Expr { expr, .. } => {
                        // Non-aggregate item: resolve against the first row
                        // of the group. Invariant within the group either
                        // because (a) implicit GROUP BY made `expr` part of
                        // the key, or (b) explicit GROUP BY listed it (the
                        // typechecker / caller is responsible for verifying
                        // this; a violation produces a possibly-arbitrary
                        // value rather than panicking).
                        let mu = match row_idxs.first() {
                            Some(&i) => &rows[i].assignment,
                            None => return Value::Str("NULL".into()),
                        };
                        match self.run_expr(mu, expr) {
                            ExprResult::Success(v) => v,
                            ExprResult::Failure(_) => Value::Str("NULL".into()),
                        }
                    }
                    ReturnItem::Aggregate { agg, .. } => self.apply_aggregator(agg, row_idxs, rows),
                })
                .collect();
            out.push(row_vals);
        }
        out
    }

    /// Reduce an aggregator over a group of rows. The runtime evaluates
    /// the inner expression once per row, applies ISO null-elimination
    /// (Failure → drop), and reduces according to the kind.
    ///
    /// All five core kinds (Count/Sum/Avg/Min/Max) plus `CountStar` are
    /// wired up. Each general-set kind shares the same prefix of work
    /// via `collect_aggregate_values` (eval + null-elim + optional
    /// distinct dedup) and differs only in the reduction step.
    fn apply_aggregator(&self, agg: &Aggregator, row_idxs: &[usize], rows: &[ResultRow]) -> Value {
        match agg {
            // ISO §20.9 GR 2: COUNT(*) is the cardinality of the group's row set.
            // No null-elimination, no DISTINCT — every row counts.
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

    /// Evaluate an aggregate's inner expression once per row, applying
    /// ISO null-elimination (Failure → drop) and, when the quantifier is
    /// DISTINCT, deduping the surviving values via a `HashSet<GroupKey>`
    /// (O(n) total, vs O(n²) for the previous Vec::contains scan).
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
            PathPattern::Node(_) => self.run_node_pattern(p),
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
            PathPattern::Repeat { pattern, lb, ub } => {
                let ub = ub.expect("unbounded repeat not supported");
                let mut acc = IntermediateResult::empty();
                for i in *lb..=ub {
                    acc = acc.union(self.run_repetition_pattern(pattern, i));
                    if self.limit_reached(&acc.rows, limit) {
                        acc.rows.truncate(limit);
                        break;
                    }
                }
                acc
            }
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
        }
    }

    // --- Optimized node pattern: uses label index when available ---

    fn run_node_pattern(&self, p: &PathPattern) -> IntermediateResult {
        let desc = p.descriptor();
        let candidates = self.get_candidate_nodes(desc);
        let var = desc.and_then(|d| d.var.as_deref());

        let rows: Vec<ResultRow> = candidates
            .iter()
            .filter(|id| self.filter_node(**id, desc))
            .map(|&id| {
                let pv = PathValue::Node(id);
                ResultRow::new(Path(vec![pv.clone()]), Assignment::from_optional(var, pv))
            })
            .collect();
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
        // Try LTJ for multi-way joins
        let join_pattern = PathPattern::Join(Box::new(q1.clone()), Box::new(q2.clone()));
        let index = TripleIndex::from_graph(self.graph);
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
        // Try LTJ for chains of directed edges
        let concat_pattern = PathPattern::Concat(Box::new(p1.clone()), Box::new(p2.clone()));
        let index = TripleIndex::from_graph(self.graph);
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
                let actual_label = self.graph.node_labels(id);
                let actual_props = Self::check_record(&self.graph.node_props(id));
                let actual = DescriptorType::new(actual_label.clone(), actual_props);
                DescriptorType::is_subtype(&actual, &d.dtype)
            }
        }
    }

    fn filter_edge(&self, id: Id, desc: Option<&Descriptor>) -> bool {
        match desc {
            None => true,
            Some(d) => {
                let actual_label = self.graph.edge_labels(id);
                let actual_props = Self::check_record(&self.graph.edge_props(id));
                let actual = DescriptorType::new(actual_label.clone(), actual_props);
                DescriptorType::is_subtype(&actual, &d.dtype)
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
        }
    }

    fn run_expr(&self, mu: &Assignment, expr: &Expr) -> ExprResult {
        match expr {
            Expr::Const(v) => ExprResult::Success(v.clone()),

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

            Expr::Type(_) => ExprResult::Failure("bare type in expression".into()),
        }
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

// =======================================================================
// Aggregate reducers (ISO §20.9 General Rules 7a-iii through 7a-vi)
//
// These run on the values that survived ISO null-elimination + optional
// DISTINCT (see Runtime::collect_aggregate_values). Each returns the
// per-group aggregate value.
//
// Empty input handling: per ISO §20.9 GR 7a-ii, SUM/AVG/MIN/MAX over an
// empty value collection yield null. gqlite represents the runtime "null"
// as `Value::Str("NULL")` to stay consistent with how `run_expr` reports
// missing attributes today; introducing a proper `Value::Null` is a
// follow-up that touches multiple subsystems.
// =======================================================================

const NULL_SENTINEL: &str = "NULL";

fn null_value() -> Value {
    Value::Str(NULL_SENTINEL.into())
}

/// SUM: integer-preserving when all inputs are Int, promotes to Float
/// when any input is Float. Non-numeric values are skipped (gradual
/// typing tolerance — strict checking is a typechecker concern, not
/// runtime). Empty input → null.
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

/// AVG: always Float (averages of integers are usually not integers).
/// Computed as sum/count over the numeric survivors. Empty → null.
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

/// MIN: smallest value by `value_cmp`. Values incomparable with the
/// running best are skipped (gradual tolerance). Empty → null.
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

/// MAX: symmetric to MIN.
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

/// Dedupe a result table preserving first-seen order. O(n) using a
/// HashSet of GroupKeys for membership; the output Vec maintains
/// insertion order. Replaces the older O(n²) `Vec::contains` scan.
fn dedup_preserving_order(rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    let mut seen: HashSet<GroupKey> = HashSet::with_capacity(rows.len());
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
    for row in rows {
        // Build the key from a clone of the row so we can keep the row
        // for the output. The clone is unavoidable here without
        // restructuring the function signature; in practice rows are
        // small (typically a handful of columns).
        let key = GroupKey::from_values(row.clone());
        if seen.insert(key) {
            out.push(row);
        }
    }
    out
}

// =======================================================================
// GroupKey: a Hash + Eq wrapper around `Vec<Value>` for grouping/dedup
//
// `Value` itself doesn't derive `Hash`/`Eq` because it contains `f64`,
// which has neither (NaN != NaN per IEEE 754, and no Hash impl exists).
// Modifying `Value` would touch ~30 sites repo-wide; instead this
// wrapper is local to the runtime and only used as a HashMap key.
//
// Float normalization for hashing:
//  - `NaN` → canonical NaN (any NaN payload hashes the same).
//  - `-0.0` → `+0.0` (so `+0.0` and `-0.0` group together; matches `==`).
//
// Equality and hash are kept consistent: two GroupKeys with equal hashes
// can be unequal (collisions are fine), but equal GroupKeys MUST hash
// the same. The normalization above is what makes that hold for floats.
// =======================================================================

/// Wrapper that gives `Vec<Value>` a `Hash + Eq` impl for use as a
/// HashMap/HashSet key. Owns its data so it can outlive the rows it was
/// built from.
#[derive(Debug, Clone)]
struct GroupKey(Vec<Value>);

/// Hash one `Value` into the given hasher with float normalization.
fn hash_value<H: Hasher>(v: &Value, state: &mut H) {
    std::mem::discriminant(v).hash(state);
    match v {
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
    }
}

/// Equality between two `Value`s using bit-equality for floats AFTER
/// normalization. Matches `hash_value` so the Hash/Eq contract holds.
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
                    .all(|(k, vx)| y.get(k).map_or(false, |vy| eq_value(vx, vy)))
        }
        _ => false,
    }
}

/// Map a float to a hashable bit pattern. `NaN`s collapse to one
/// canonical NaN; `-0.0` collapses to `+0.0`. The result preserves
/// `==`-equivalence: two floats that compare equal under our scheme
/// (NaN-equal-to-NaN, +0/-0 equal) hash to the same bits.
fn normalize_float_bits(f: f64) -> u64 {
    if f.is_nan() {
        f64::NAN.to_bits()
    } else if f == 0.0 {
        // both +0.0 and -0.0 compare == to 0.0 under IEEE; collapse to +0.0.
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

/// Total ordering between `Value`s for MIN/MAX. Mixes Int<->Float by
/// promotion; comparisons across unrelated kinds (e.g. Int vs Str)
/// return None and the caller skips them. List/Record never compare.
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
