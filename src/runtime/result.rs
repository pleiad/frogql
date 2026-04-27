use std::fmt;

use super::assignment::Assignment;
use crate::model::value::{Path, Value};

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

/// Collection of result rows from pattern evaluation.
#[derive(Debug, Clone)]
pub struct IntermediateResult {
    pub rows: Vec<ResultRow>,
}

impl IntermediateResult {
    pub fn new(rows: Vec<ResultRow>) -> Self {
        Self { rows }
    }

    pub fn empty() -> Self {
        Self { rows: vec![] }
    }

    pub fn union(mut self, other: IntermediateResult) -> IntermediateResult {
        self.rows.extend(other.rows);
        self
    }

    /// Wrap each assignment into singleton lists for grouping.
    pub fn to_group(&self) -> IntermediateResult {
        IntermediateResult {
            rows: self
                .rows
                .iter()
                .map(|r| {
                    let mut mu = r.assignment.clone();
                    mu.to_group();
                    ResultRow::with_paths(r.paths.clone(), mu)
                })
                .collect(),
        }
    }
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
