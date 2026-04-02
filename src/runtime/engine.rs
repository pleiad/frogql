use std::collections::HashMap;

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Path, PathValue, Value};
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::typing::descriptor_type::DescriptorType;
use crate::typing::property_type::PropertyType;
use crate::typing::simple_type::SimpleType;

use super::assignment::Assignment;
use super::result::{ExprResult, IntermediateResult, ResultRow};

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
        self.run_path_pattern(pattern)
    }

    fn run_path_pattern(&self, p: &PathPattern) -> IntermediateResult {
        match p {
            PathPattern::Node(_) => self.run_node_pattern(p),
            PathPattern::EdgeRight(_)
            | PathPattern::EdgeLeft(_)
            | PathPattern::EdgeUndirected(_)
            | PathPattern::EdgeAnyDirection(_) => self.run_edge_pattern(p),
            PathPattern::Concat(p1, p2) => self.run_concat_pattern(p1, p2),
            PathPattern::Union(p1, p2) => {
                let ir1 = self.run_path_pattern(p1);
                let ir2 = self.run_path_pattern(p2);
                let dom = p.freevars();
                let mut rows = Vec::new();
                for mut r in ir1.rows.into_iter().chain(ir2.rows) {
                    r.assignment.fill_nones(&dom);
                    rows.push(r);
                }
                IntermediateResult::new(rows)
            }
            PathPattern::Filter(inner, expr) => {
                let ir = self.run_path_pattern(inner);
                let rows = ir
                    .rows
                    .into_iter()
                    .filter(|r| self.run_expr(&r.assignment, expr).get_bool())
                    .collect();
                IntermediateResult::new(rows)
            }
            PathPattern::Repeat { pattern, lb, ub } => {
                let ub = ub.expect("unbounded repeat not supported");
                let mut acc = IntermediateResult::empty();
                for i in *lb..=ub {
                    acc = acc.union(self.run_repetition_pattern(pattern, i));
                }
                acc
            }
            PathPattern::Questioned(inner) => {
                let ir_empty = self.run_path_pattern(&PathPattern::Node(None));
                let ir_inner = self.run_path_pattern(inner);
                ir_empty.union(ir_inner)
            }
        }
    }

    // --- Optimized node pattern: uses label index when available ---

    fn run_node_pattern(&self, p: &PathPattern) -> IntermediateResult {
        let desc = p.descriptor();
        let candidates = self.get_candidate_nodes(desc);
        let var = desc.and_then(|d| d.var.as_deref());

        let rows: Vec<ResultRow> = candidates
            .iter()
            .filter(|id| self.filter_element(id, desc))
            .map(|id| {
                let pv = PathValue::Node(id.clone());
                ResultRow::new(
                    Path(vec![pv.clone()]),
                    Assignment::from_optional(var, pv),
                )
            })
            .collect();
        IntermediateResult::new(rows)
    }

    /// Get candidate node IDs — uses label index if descriptor has a simple label.
    fn get_candidate_nodes(&self, desc: Option<&Descriptor>) -> Vec<String> {
        if let Some(label) = Self::extract_simple_label(desc) {
            if let Some(indexed) = self.graph.nodes_with_label(label) {
                return indexed;
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
                    .filter(|id| self.filter_element(id, desc.as_ref()))
                    .map(|eid| {
                        let edge_pv = PathValue::EdgeDirectional(eid.clone());
                        let (first, last) = if is_right {
                            (
                                PathValue::Node(self.graph.src(eid).to_string()),
                                PathValue::Node(self.graph.tgt(eid).to_string()),
                            )
                        } else {
                            (
                                PathValue::Node(self.graph.tgt(eid).to_string()),
                                PathValue::Node(self.graph.src(eid).to_string()),
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
                for eid in candidates.iter().filter(|id| self.filter_element(id, desc.as_ref())) {
                    let edge_pv = PathValue::EdgeUndirectional(eid.clone());
                    let (ep0, ep1) = self.graph.endpoints(eid);
                    rows.push(ResultRow::new(
                        Path(vec![PathValue::Node(ep0.to_string()), edge_pv.clone(), PathValue::Node(ep1.to_string())]),
                        Assignment::from_optional(var, edge_pv.clone()),
                    ));
                    rows.push(ResultRow::new(
                        Path(vec![PathValue::Node(ep1.to_string()), edge_pv.clone(), PathValue::Node(ep0.to_string())]),
                        Assignment::from_optional(var, edge_pv),
                    ));
                }
                IntermediateResult::new(rows)
            }
            _ => unreachable!(),
        }
    }

    fn get_candidate_directed_edges(&self, desc: Option<&Descriptor>) -> Vec<String> {
        if let Some(label) = Self::extract_simple_label(desc) {
            if let Some(indexed) = self.graph.directed_edges_with_label(label) {
                return indexed;
            }
        }
        self.graph.edges_directed()
    }

    fn get_candidate_undirected_edges(&self, desc: Option<&Descriptor>) -> Vec<String> {
        if let Some(label) = Self::extract_simple_label(desc) {
            if let Some(indexed) = self.graph.undirected_edges_with_label(label) {
                return indexed;
            }
        }
        self.graph.edges_undirected()
    }

    // --- Optimized concatenation: uses adjacency when right side is edge/node ---

    fn run_concat_pattern(&self, p1: &PathPattern, p2: &PathPattern) -> IntermediateResult {
        let ir1 = self.run_path_pattern(p1);

        // Optimization: if p2 is a simple edge or node pattern, use adjacency-driven execution
        match p2 {
            PathPattern::EdgeRight(desc) => {
                return self.concat_with_directed_edge(&ir1, desc.as_ref(), true);
            }
            PathPattern::EdgeLeft(desc) => {
                return self.concat_with_directed_edge(&ir1, desc.as_ref(), false);
            }
            PathPattern::EdgeUndirected(desc) => {
                return self.concat_with_undirected_edge(&ir1, desc.as_ref());
            }
            PathPattern::EdgeAnyDirection(desc) => {
                let right = self.concat_with_directed_edge(&ir1, desc.as_ref(), true);
                let left = self.concat_with_directed_edge(&ir1, desc.as_ref(), false);
                let und = self.concat_with_undirected_edge(&ir1, desc.as_ref());
                return right.union(left).union(und);
            }
            PathPattern::Node(desc) => {
                return self.concat_with_node(&ir1, desc.as_ref());
            }
            // For Filter wrapping an edge pattern, we can still optimize the edge part
            PathPattern::Filter(inner, expr) => {
                if let Some(optimized) = self.try_concat_with_filtered_edge(&ir1, inner, expr) {
                    return optimized;
                }
            }
            _ => {}
        }

        // Fallback: cross-product for complex right-side patterns
        let ir2 = self.run_path_pattern(p2);
        Self::hash_join(&ir1, &ir2)
    }

    /// Adjacency-driven concat: left results → outgoing/incoming edges → target nodes.
    fn concat_with_directed_edge(
        &self, ir1: &IntermediateResult, desc: Option<&Descriptor>, is_right: bool,
    ) -> IntermediateResult {
        let var = desc.and_then(|d| d.var.as_deref());
        let mut rows = Vec::new();

        for r1 in &ir1.rows {
            let Some(last_node) = r1.path.last_node_id() else { continue };

            let edge_ids = if is_right {
                self.graph.outgoing_edges(last_node)
            } else {
                self.graph.incoming_edges(last_node)
            };

            for eid in &edge_ids {
                if !self.filter_element(eid, desc) {
                    continue;
                }
                let edge_pv = PathValue::EdgeDirectional(eid.clone());
                let other_node = if is_right {
                    self.graph.tgt(eid)
                } else {
                    self.graph.src(eid)
                };
                let edge_mu = Assignment::from_optional(var, edge_pv.clone());
                if !r1.assignment.can_unify(&edge_mu) {
                    continue;
                }
                let new_path = Path(vec![edge_pv, PathValue::Node(other_node.to_string())]);
                rows.push(ResultRow::new(
                    r1.path.concat(&Path(vec![
                        PathValue::Node(last_node.to_string()),
                        new_path.0[0].clone(),
                        new_path.0[1].clone(),
                    ])),
                    r1.assignment.unify(&edge_mu),
                ));
            }
        }
        IntermediateResult::new(rows)
    }

    fn concat_with_undirected_edge(
        &self, ir1: &IntermediateResult, desc: Option<&Descriptor>,
    ) -> IntermediateResult {
        let var = desc.and_then(|d| d.var.as_deref());
        let mut rows = Vec::new();

        for r1 in &ir1.rows {
            let Some(last_node) = r1.path.last_node_id() else { continue };

            for eid in &self.graph.undirected_edges_of(last_node) {
                if !self.filter_element(eid, desc) {
                    continue;
                }
                let edge_pv = PathValue::EdgeUndirectional(eid.clone());
                let (ep0, ep1) = self.graph.endpoints(eid);
                let other_node = if ep0 == last_node { ep1 } else { ep0 };

                let edge_mu = Assignment::from_optional(var, edge_pv.clone());
                if !r1.assignment.can_unify(&edge_mu) {
                    continue;
                }
                rows.push(ResultRow::new(
                    r1.path.concat(&Path(vec![
                        PathValue::Node(last_node.to_string()),
                        edge_pv.clone(),
                        PathValue::Node(other_node.to_string()),
                    ])),
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
            }
        }
        IntermediateResult::new(rows)
    }

    /// Adjacency-driven concat with a node pattern: just check if the last node matches.
    fn concat_with_node(
        &self, ir1: &IntermediateResult, desc: Option<&Descriptor>,
    ) -> IntermediateResult {
        let var = desc.and_then(|d| d.var.as_deref());
        let mut rows = Vec::new();

        for r1 in &ir1.rows {
            let Some(last_node) = r1.path.last_node_id() else { continue };
            if !self.filter_element(last_node, desc) {
                continue;
            }
            let node_pv = PathValue::Node(last_node.to_string());
            let node_mu = Assignment::from_optional(var, node_pv.clone());
            if !r1.assignment.can_unify(&node_mu) {
                continue;
            }
            // Node concat: the last node already IS the node, so path doesn't grow
            rows.push(ResultRow::new(
                r1.path.clone(),
                r1.assignment.unify(&node_mu),
            ));
        }
        IntermediateResult::new(rows)
    }

    /// Try to optimize concat with Filter(edge_pattern, expr).
    fn try_concat_with_filtered_edge(
        &self, ir1: &IntermediateResult, inner: &PathPattern, expr: &Expr,
    ) -> Option<IntermediateResult> {
        match inner {
            PathPattern::EdgeRight(desc) => {
                let ir = self.concat_with_directed_edge(ir1, desc.as_ref(), true);
                Some(self.apply_filter(ir, expr))
            }
            PathPattern::EdgeLeft(desc) => {
                let ir = self.concat_with_directed_edge(ir1, desc.as_ref(), false);
                Some(self.apply_filter(ir, expr))
            }
            PathPattern::EdgeUndirected(desc) => {
                let ir = self.concat_with_undirected_edge(ir1, desc.as_ref());
                Some(self.apply_filter(ir, expr))
            }
            PathPattern::EdgeAnyDirection(desc) => {
                let r = self.concat_with_directed_edge(ir1, desc.as_ref(), true);
                let l = self.concat_with_directed_edge(ir1, desc.as_ref(), false);
                let u = self.concat_with_undirected_edge(ir1, desc.as_ref());
                Some(self.apply_filter(r.union(l).union(u), expr))
            }
            _ => None,
        }
    }

    fn apply_filter(&self, ir: IntermediateResult, expr: &Expr) -> IntermediateResult {
        IntermediateResult::new(
            ir.rows.into_iter()
                .filter(|r| self.run_expr(&r.assignment, expr).get_bool())
                .collect()
        )
    }

    /// Hash-join on the concatenation key (last node of ir1 = first node of ir2).
    /// O(n + m) expected instead of O(n × m) cross-product.
    fn hash_join(ir1: &IntermediateResult, ir2: &IntermediateResult) -> IntermediateResult {
        // Build hash map: first_node_id → Vec<index into ir2.rows>
        let mut ir2_by_first: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, r2) in ir2.rows.iter().enumerate() {
            if let Some(first) = r2.path.first_node_id() {
                ir2_by_first.entry(first).or_default().push(i);
            }
        }

        let mut rows = Vec::new();
        for r1 in &ir1.rows {
            let Some(last) = r1.path.last_node_id() else { continue };
            let Some(matches) = ir2_by_first.get(last) else { continue };
            for &idx in matches {
                let r2 = &ir2.rows[idx];
                if r1.assignment.can_unify(&r2.assignment) {
                    rows.push(ResultRow::new(
                        r1.path.concat(&r2.path),
                        r1.assignment.unify(&r2.assignment),
                    ));
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
                .map(|id| ResultRow::new(Path(vec![PathValue::Node(id.clone())]), mu.clone()))
                .collect();
            return IntermediateResult::new(rows);
        }

        let ir = self.run_path_pattern(p);
        let grouped = ir.to_group();

        if n == 1 {
            return grouped;
        }

        // Build hash map once: first_node_id → indices in grouped.rows
        let mut grouped_by_first: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, r) in grouped.rows.iter().enumerate() {
            if let Some(first) = r.path.first_node_id() {
                grouped_by_first.entry(first).or_default().push(i);
            }
        }

        let mut res = grouped.clone();
        for _ in 1..n {
            let mut new_rows = Vec::new();
            for r in &res.rows {
                let Some(last) = r.path.last_node_id() else { continue };
                let Some(matches) = grouped_by_first.get(last) else { continue };
                for &idx in matches {
                    new_rows.push(r.concat_group(&grouped.rows[idx]));
                }
            }
            res = IntermediateResult::new(new_rows);
        }
        res
    }

    // --- Helpers ---

    /// Extract a simple label name from a descriptor, if it's just `Label(name)`.
    fn extract_simple_label(desc: Option<&Descriptor>) -> Option<&str> {
        desc?.dtype.label.as_simple_label()
    }

    fn filter_element(&self, id: &str, desc: Option<&Descriptor>) -> bool {
        match desc {
            None => true,
            Some(d) => self.select(&d.dtype, id),
        }
    }

    fn select(&self, dtype: &DescriptorType, id: &str) -> bool {
        let actual_label = self.graph.labels(id);
        let actual_props = Self::check_record(self.graph.props(id));
        let actual = DescriptorType::new(actual_label.clone(), actual_props);
        DescriptorType::is_subtype(&actual, dtype)
    }

    fn check_record(props: &Props) -> PropertyType {
        let mut m = std::collections::BTreeMap::new();
        for (k, v) in props {
            let t = match v {
                Value::Int(_) => SimpleType::Z,
                Value::Str(_) => SimpleType::S,
                Value::Bool(_) => SimpleType::B,
            };
            m.insert(k.clone(), t);
        }
        PropertyType::Closed(m)
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
                match self.graph.props(id).get(attr) {
                    Some(v) => ExprResult::Success(v.clone()),
                    None => ExprResult::Failure(format!("attribute '{attr}' not found")),
                }
            }

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
                            _ => ExprResult::Failure("neg requires int".into()),
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
        match op {
            BinOp::Add => match (lv, rv) {
                (Value::Int(a), Value::Int(b)) => ExprResult::Success(Value::Int(a + b)),
                _ => ExprResult::Failure("+ requires ints".into()),
            },
            BinOp::Sub => match (lv, rv) {
                (Value::Int(a), Value::Int(b)) => ExprResult::Success(Value::Int(a - b)),
                _ => ExprResult::Failure("- requires ints".into()),
            },
            BinOp::Gt => match (lv, rv) {
                (Value::Int(a), Value::Int(b)) => ExprResult::Success(Value::Bool(a > b)),
                _ => ExprResult::Failure("> requires ints".into()),
            },
            BinOp::Lt => match (lv, rv) {
                (Value::Int(a), Value::Int(b)) => ExprResult::Success(Value::Bool(a < b)),
                _ => ExprResult::Failure("< requires ints".into()),
            },
            BinOp::Ge => match (lv, rv) {
                (Value::Int(a), Value::Int(b)) => ExprResult::Success(Value::Bool(a >= b)),
                _ => ExprResult::Failure(">= requires ints".into()),
            },
            BinOp::Le => match (lv, rv) {
                (Value::Int(a), Value::Int(b)) => ExprResult::Success(Value::Bool(a <= b)),
                _ => ExprResult::Failure("<= requires ints".into()),
            },
            BinOp::Eq => ExprResult::Success(Value::Bool(lv == rv)),
            BinOp::Ne => ExprResult::Success(Value::Bool(lv != rv)),
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
