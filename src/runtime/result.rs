use std::fmt;

use crate::model::value::{Path, Value};
use super::assignment::Assignment;

/// A single result row: a matched path + variable bindings.
#[derive(Debug, Clone)]
pub struct ResultRow {
    pub path: Path,
    pub assignment: Assignment,
}

impl ResultRow {
    pub fn new(path: Path, assignment: Assignment) -> Self {
        Self { path, assignment }
    }

    /// Concatenate two grouped result rows.
    pub fn concat_group(&self, other: &ResultRow) -> ResultRow {
        ResultRow {
            path: self.path.concat(&other.path),
            assignment: self.assignment.concat_group(&other.assignment),
        }
    }
}

impl fmt::Display for ResultRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.path, self.assignment)
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
                    ResultRow::new(r.path.clone(), mu)
                })
                .collect(),
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
