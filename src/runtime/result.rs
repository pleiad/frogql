use std::fmt;

use super::assignment::Assignment;
use crate::model::value::{Path, PathValue, Value};

/// A single result row: a tuple of matched paths + variable bindings.
///
/// A simple pattern like `()-[]->()` produces a single path: `paths = [p]`.
/// A join `Q1, Q2` produces the concatenation of path tuples: `paths = [...p̄₁, ...p̄₂]`.
/// This matches the paper semantics where `⟦Q1, Q2⟧` yields `(p̄₁ × p̄₂, μ₁ ∪ μ₂)`.
#[derive(Debug, Clone)]
pub struct ResultRow {
    pub paths: Vec<Path>,
    pub assignment: Assignment,
}

impl ResultRow {
    pub fn new(path: Path, assignment: Assignment) -> Self {
        Self {
            paths: vec![path],
            assignment,
        }
    }

    pub fn with_paths(paths: Vec<Path>, assignment: Assignment) -> Self {
        Self { paths, assignment }
    }

    /// The "current" (last) path — used by concat/repetition to extend.
    pub fn path(&self) -> &Path {
        self.paths.last().unwrap()
    }

    /// Concatenate two grouped result rows.
    pub fn concat_group(&self, other: &ResultRow) -> ResultRow {
        // For concat_group (repetition), we extend the last path
        let mut paths = self.paths.clone();
        if let Some(last) = paths.last_mut() {
            *last = last.concat(other.path());
        }
        ResultRow {
            paths,
            assignment: self.assignment.concat_group(&other.assignment),
        }
    }

    /// Extend the last path in this row by concatenating with `extension`.
    /// Used by concat operations (edge/node append).
    pub fn extend_path(&self, extension: &Path, assignment: Assignment) -> ResultRow {
        let mut paths = self.paths.clone();
        if let Some(last) = paths.last_mut() {
            *last = last.concat(extension);
        }
        ResultRow { paths, assignment }
    }

    /// Like extend_path but replaces the last path entirely (for node concat where path doesn't grow).
    pub fn with_same_paths(&self, assignment: Assignment) -> ResultRow {
        ResultRow {
            paths: self.paths.clone(),
            assignment,
        }
    }

    /// Join two result rows: concatenate path tuples.
    pub fn join(r1: &ResultRow, r2: &ResultRow, assignment: Assignment) -> ResultRow {
        let mut paths = r1.paths.clone();
        paths.extend(r2.paths.iter().cloned());
        ResultRow { paths, assignment }
    }
}

impl fmt::Display for ResultRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path_strs: Vec<String> = self.paths.iter().map(|p| format!("{}", p)).collect();
        write!(f, "{} {}", path_strs.join(", "), self.assignment)
    }
}

use crate::model::value::Id;
use std::collections::HashMap;

/// Compressed query intermediate results using factorized execution tree representation.
#[derive(Debug, Clone)]
pub enum FactorNode {
    Flat(Vec<ResultRow>),
    Product(Vec<FactorNode>),
    PathConcat(Vec<FactorNode>),
    Union(Vec<FactorNode>),
}

impl FactorNode {
    pub fn is_empty(&self) -> bool {
        match self {
            FactorNode::Flat(rows) => rows.is_empty(),
            FactorNode::Union(sub) => sub.iter().all(|s| s.is_empty()),
            FactorNode::Product(sub) | FactorNode::PathConcat(sub) => {
                sub.iter().any(|s| s.is_empty())
            }
        }
    }

    pub fn flatten(&self) -> Vec<ResultRow> {
        match self {
            FactorNode::Flat(rows) => rows.clone(),
            FactorNode::Union(sub) => {
                let dom = self.freevars();
                let mut rows = Vec::new();
                for s in sub {
                    for mut r in s.flatten() {
                        r.assignment.fill_nones(&dom);
                        rows.push(r);
                    }
                }
                rows
            }
            FactorNode::Product(sub) => {
                if sub.is_empty() {
                    return Vec::new();
                }
                let mut res = sub[0].flatten();
                for s in &sub[1..] {
                    res = join_flat_rows_variable(&res, &s.flatten());
                }
                res
            }
            FactorNode::PathConcat(sub) => {
                if sub.is_empty() {
                    return Vec::new();
                }
                let mut res = sub[0].flatten();
                for s in &sub[1..] {
                    res = join_flat_rows_endpoint(&res, &s.flatten());
                }
                res
            }
        }
    }

    pub fn freevars(&self) -> std::collections::HashSet<String> {
        match self {
            FactorNode::Flat(rows) => {
                rows.first().map(|r| r.assignment.keys()).unwrap_or_default()
            }
            FactorNode::Union(sub) | FactorNode::Product(sub) | FactorNode::PathConcat(sub) => {
                let mut vars = std::collections::HashSet::new();
                for s in sub {
                    vars.extend(s.freevars());
                }
                vars
            }
        }
    }

    pub fn partition(self, x: &str) -> std::collections::HashMap<PathValue, FactorNode> {
        use crate::model::value::PathValue;
        let mut map = std::collections::HashMap::new();
        match self {
            FactorNode::Flat(rows) => {
                let mut groups: std::collections::HashMap<PathValue, Vec<ResultRow>> = std::collections::HashMap::new();
                for r in rows {
                    if let Some(pv) = r.assignment.get(x) {
                        groups.entry(pv.clone()).or_default().push(r);
                    }
                }
                for (pv, group_rows) in groups {
                    map.insert(pv, FactorNode::Flat(group_rows));
                }
            }
            FactorNode::Union(sub) => {
                let mut groups: std::collections::HashMap<PathValue, Vec<FactorNode>> = std::collections::HashMap::new();
                for s in sub {
                    for (pv, part) in s.partition(x) {
                        groups.entry(pv).or_default().push(part);
                    }
                }
                for (pv, group_nodes) in groups {
                    map.insert(pv, FactorNode::Union(group_nodes));
                }
            }
            FactorNode::Product(sub) => {
                let mut target_idx = None;
                for (i, s) in sub.iter().enumerate() {
                    if s.freevars().contains(x) {
                        target_idx = Some(i);
                        break;
                    }
                }
                if let Some(idx) = target_idx {
                    let mut sub_cloned = sub.clone();
                    let target_node = sub_cloned.remove(idx);
                    let target_parts = target_node.partition(x);
                    for (pv, part) in target_parts {
                        let mut product_subs = sub_cloned.clone();
                        product_subs.insert(idx, part);
                        map.insert(pv, FactorNode::Product(product_subs));
                    }
                }
            }
            FactorNode::PathConcat(sub) => {
                let mut target_idx = None;
                for (i, s) in sub.iter().enumerate() {
                    if s.freevars().contains(x) {
                        target_idx = Some(i);
                        break;
                    }
                }
                if let Some(idx) = target_idx {
                    let mut sub_cloned = sub.clone();
                    let target_node = sub_cloned.remove(idx);
                    let target_parts = target_node.partition(x);
                    for (pv, part) in target_parts {
                        let mut product_subs = sub_cloned.clone();
                        product_subs.insert(idx, part);
                        map.insert(pv, FactorNode::PathConcat(product_subs));
                    }
                }
            }
        }
        map
    }

    pub fn join(self, other: FactorNode, join_var: &str) -> FactorNode {
        let left_parts = self.partition(join_var);
        let right_parts = other.partition(join_var);

        let mut unions = Vec::new();
        let mut sorted_keys: Vec<_> = left_parts.keys().collect();
        sorted_keys.sort_by(|a, b| format!("{:?}", a).cmp(&format!("{:?}", b)));

        for val in sorted_keys {
            if let Some(left_node) = left_parts.get(val) {
                if let Some(right_node) = right_parts.get(val) {
                    unions.push(FactorNode::Product(vec![left_node.clone(), right_node.clone()]));
                }
            }
        }
        FactorNode::Union(unions)
    }
}

pub fn join_flat_rows_variable(left: &[ResultRow], right: &[ResultRow]) -> Vec<ResultRow> {
    if left.is_empty() || right.is_empty() {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for r1 in left {
        for r2 in right {
            if r1.assignment.can_unify(&r2.assignment) {
                rows.push(ResultRow::join(r1, r2, r1.assignment.unify(&r2.assignment)));
            }
        }
    }
    rows
}

pub fn join_flat_rows_endpoint(left: &[ResultRow], right: &[ResultRow]) -> Vec<ResultRow> {
    if left.is_empty() || right.is_empty() {
        return Vec::new();
    }
    let mut ir2_by_first: HashMap<Id, Vec<usize>> = HashMap::new();
    for (i, r2) in right.iter().enumerate() {
        if let Some(first) = r2.path().first_node_id() {
            ir2_by_first.entry(first).or_default().push(i);
        }
    }

    let mut rows = Vec::new();
    for r1 in left {
        if let Some(last) = r1.path().last_node_id() {
            if let Some(matches) = ir2_by_first.get(&last) {
                for &idx in matches {
                    let r2 = &right[idx];
                    if r1.assignment.can_unify(&r2.assignment) {
                        rows.push(r1.extend_path(r2.path(), r1.assignment.unify(&r2.assignment)));
                    }
                }
            }
        }
    }
    rows
}

/// Collection of result rows from pattern evaluation (factorized tree representation).
#[derive(Debug, Clone)]
pub struct IntermediateResult {
    pub rows: Vec<ResultRow>,
    pub root: Option<FactorNode>,
}

impl IntermediateResult {
    pub fn new(rows: Vec<ResultRow>) -> Self {
        Self {
            rows: rows.clone(),
            root: Some(FactorNode::Flat(rows)),
        }
    }

    pub fn empty() -> Self {
        Self {
            rows: vec![],
            root: Some(FactorNode::Flat(vec![])),
        }
    }

    pub fn from_node(root: FactorNode) -> Self {
        Self {
            rows: vec![],
            root: Some(root),
        }
    }

    pub fn ensure_flat(&mut self) {
        if self.rows.is_empty() {
            if let Some(ref root) = self.root {
                self.rows = root.flatten();
            }
        }
    }

    pub fn into_root(self) -> FactorNode {
        self.root.unwrap_or(FactorNode::Flat(self.rows))
    }

    pub fn is_empty(&self) -> bool {
        if let Some(ref root) = self.root {
            root.is_empty()
        } else {
            self.rows.is_empty()
        }
    }

    pub fn freevars(&self) -> std::collections::HashSet<String> {
        if let Some(ref root) = self.root {
            root.freevars()
        } else {
            self.rows.first().map(|r| r.assignment.keys()).unwrap_or_default()
        }
    }

    pub fn union(self, other: IntermediateResult) -> IntermediateResult {
        IntermediateResult::from_node(FactorNode::Union(vec![
            self.into_root(),
            other.into_root(),
        ]))
    }

    /// Wrap each assignment into singleton lists for grouping.
    pub fn to_group(&self) -> IntermediateResult {
        let mut flat = self.clone();
        flat.ensure_flat();
        IntermediateResult {
            rows: flat
                .rows
                .iter()
                .map(|r| {
                    let mut mu = r.assignment.clone();
                    mu.to_group();
                    ResultRow::with_paths(r.paths.clone(), mu)
                })
                .collect(),
            root: None,
        }
    }
}

/// Compressed query intermediate results using factorized execution tree representation.
#[derive(Debug, Clone)]
pub enum FactorizedResult {
    Flat(IntermediateResult),
    Product(Vec<FactorizedResult>),
    Union(Vec<FactorizedResult>),
}

impl FactorizedResult {
    pub fn flatten(&self) -> IntermediateResult {
        match self {
            FactorizedResult::Flat(ir) => ir.clone(),
            FactorizedResult::Union(sub) => {
                let mut res = IntermediateResult::empty();
                for s in sub {
                    res = res.union(s.flatten());
                }
                res
            }
            FactorizedResult::Product(sub) => {
                if sub.is_empty() {
                    return IntermediateResult::empty();
                }
                let mut res = sub[0].flatten();
                for s in &sub[1..] {
                    res = join_intermediate_results(&res, &s.flatten());
                }
                res
            }
        }
    }
}

pub fn join_intermediate_results(res1: &IntermediateResult, res2: &IntermediateResult) -> IntermediateResult {
    let mut rows = Vec::new();
    for r1 in &res1.rows {
        for r2 in &res2.rows {
            if r1.assignment.can_unify(&r2.assignment) {
                rows.push(ResultRow::join(r1, r2, r1.assignment.unify(&r2.assignment)));
            }
        }
    }
    IntermediateResult::new(rows)
}

/// Result of a full Query execution.
#[derive(Debug)]
pub enum QueryResult {
    /// No RETURN clause — raw pattern match results.
    Raw(IntermediateResult),
    /// RETURN clause — projected rows of evaluated expressions.
    Projected(Vec<Vec<Value>>),
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        match self {
            QueryResult::Raw(ir) => ir.rows.len(),
            QueryResult::Projected(rows) => rows.len(),
        }
    }
}

/// Result of expression evaluation.
#[derive(Debug)]
pub enum ExprResult {
    Success(Value),
    Failure(String),
}

impl ExprResult {
    pub fn get_bool(&self) -> bool {
        match self {
            ExprResult::Success(Value::Bool(b)) => *b,
            _ => false,
        }
    }
}
