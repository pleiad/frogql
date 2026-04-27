use std::collections::HashMap;

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Path, PathValue, Value};
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::Query;
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
    pub fn run_query(&self, query: &Query, limit: usize) -> QueryResult {
        let ir = self.run_path_pattern(&query.pattern, limit);

        match &query.returns {
            None => QueryResult::Raw(ir),
            Some(return_items) => {
                let mut projected: Vec<Vec<Value>> = Vec::new();
                for row in &ir.rows {
                    let vals: Vec<Value> = return_items
                        .iter()
                        .map(|item| match self.run_expr(&row.assignment, &item.expr) {
                            ExprResult::Success(v) => v,
                            ExprResult::Failure(_) => Value::Str("NULL".into()),
                        })
                        .collect();
                    if query.distinct {
                        if !projected.contains(&vals) {
                            projected.push(vals);
                        }
                    } else {
                        projected.push(vals);
                    }
                }
                QueryResult::Projected(projected)
            }
        }
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
