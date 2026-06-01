//! ISO/IEC 39075:2024 §16.6 path prefix evaluation.
//!
//! A selected pattern is materialized, filtered by path mode, then reduced by
//! the path search policy per `(left boundary, right boundary)` partition.

use std::collections::{HashMap, HashSet};

use crate::model::value::Id;
use crate::runtime::result::{IntermediateResult, ResultRow};
use crate::syntax::path_prefix::{PathMode, PathPrefix, PathSearch};

/// Search partition key: `(first node id, last node id)`.
type BoundaryKey = (Option<Id>, Option<Id>);

/// Apply a `<path pattern prefix>` to one selected pattern operand's rows.
pub fn apply_path_prefix(ir: IntermediateResult, prefix: PathPrefix) -> IntermediateResult {
    if prefix.is_trivial() {
        return ir;
    }

    let mut rows: Vec<ResultRow> = ir
        .rows
        .into_iter()
        .filter(|r| row_satisfies_mode(r, prefix.mode))
        .collect();

    match prefix.search {
        PathSearch::All => {}
        PathSearch::Any { count } => select_any(&mut rows, count),
        PathSearch::ShortestPaths { count } => select_shortest_paths(&mut rows, count),
        PathSearch::ShortestGroups { count } => select_shortest_groups(&mut rows, count),
    }

    IntermediateResult::new(rows)
}

fn row_satisfies_mode(row: &ResultRow, mode: PathMode) -> bool {
    row.paths.iter().all(|p| path_satisfies_mode(p, mode))
}

/// Used both after materialization and while pruning unbounded mode search.
pub(crate) fn path_satisfies_mode(path: &crate::model::value::Path, mode: PathMode) -> bool {
    match mode {
        PathMode::Walk => true,
        PathMode::Trail => no_repeats(
            path.0
                .iter()
                .filter(|pv| pv.is_edge())
                .filter_map(|pv| pv.id()),
        ),
        PathMode::Acyclic => no_repeats(
            path.0
                .iter()
                .filter(|pv| pv.is_node())
                .filter_map(|pv| pv.id()),
        ),
        PathMode::Simple => {
            let node_ids: Vec<Id> = path
                .0
                .iter()
                .filter(|pv| pv.is_node())
                .filter_map(|pv| pv.id())
                .collect();
            if node_ids.len() <= 1 {
                return true;
            }
            let first = node_ids[0];
            let last_idx = node_ids.len() - 1;
            let mut seen = HashSet::with_capacity(node_ids.len());
            for (i, &id) in node_ids.iter().enumerate() {
                // The closing node is allowed to equal the opening node.
                if i == last_idx && id == first {
                    continue;
                }
                if !seen.insert(id) {
                    return false;
                }
            }
            true
        }
    }
}

/// True when an iterator of ids has no duplicate.
fn no_repeats(ids: impl Iterator<Item = Id>) -> bool {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return false;
        }
    }
    true
}

fn boundary_key(row: &ResultRow) -> BoundaryKey {
    let first = row.paths.first().and_then(|p| p.first_node_id());
    let last = row.paths.last().and_then(|p| p.last_node_id());
    (first, last)
}

/// Total path length of a row = number of edges across all its paths.
fn row_length(row: &ResultRow) -> usize {
    row.paths
        .iter()
        .map(|p| p.0.iter().filter(|pv| pv.is_edge()).count())
        .sum()
}

/// `ANY N`: keep up to `count` rows per boundary partition, in the order
/// the runtime produced them.
fn select_any(rows: &mut Vec<ResultRow>, count: usize) {
    let mut seen: HashMap<BoundaryKey, usize> = HashMap::new();
    rows.retain(|r| {
        let slot = seen.entry(boundary_key(r)).or_insert(0);
        if *slot < count {
            *slot += 1;
            true
        } else {
            false
        }
    });
}

/// `SHORTEST N [PATHS]`: keep the `count` shortest rows per partition,
/// ranked by length. Ties are broken by production order (stable sort).
fn select_shortest_paths(rows: &mut Vec<ResultRow>, count: usize) {
    let lengths: Vec<usize> = rows.iter().map(row_length).collect();
    let mut by_key: HashMap<BoundaryKey, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        by_key.entry(boundary_key(r)).or_default().push(i);
    }
    let mut keep = vec![false; rows.len()];
    for indices in by_key.values_mut() {
        indices.sort_by_key(|&i| lengths[i]); // stable: preserves order within a length
        for &i in indices.iter().take(count) {
            keep[i] = true;
        }
    }
    retain_marked(rows, &keep);
}

/// `SHORTEST N GROUPS`: keep every row whose length is among the `count`
/// shortest distinct lengths in its partition.
fn select_shortest_groups(rows: &mut Vec<ResultRow>, count: usize) {
    let lengths: Vec<usize> = rows.iter().map(row_length).collect();
    let mut by_key: HashMap<BoundaryKey, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        by_key.entry(boundary_key(r)).or_default().push(i);
    }
    let mut keep = vec![false; rows.len()];
    for indices in by_key.values() {
        let mut distinct: Vec<usize> = indices.iter().map(|&i| lengths[i]).collect();
        distinct.sort_unstable();
        distinct.dedup();
        distinct.truncate(count);
        let allowed: HashSet<usize> = distinct.into_iter().collect();
        for &i in indices {
            if allowed.contains(&lengths[i]) {
                keep[i] = true;
            }
        }
    }
    retain_marked(rows, &keep);
}

/// Retain rows flagged `true` in `keep`, preserving order.
fn retain_marked(rows: &mut Vec<ResultRow>, keep: &[bool]) {
    let mut iter = keep.iter();
    rows.retain(|_| *iter.next().unwrap_or(&false));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::value::{Path, PathValue};
    use crate::runtime::assignment::Assignment;

    /// Build a row from an explicit node/edge id sequence: even positions
    /// are node ids, odd positions are edge ids.
    fn path_row(elems: &[(bool, Id)]) -> ResultRow {
        let pvs: Vec<PathValue> = elems
            .iter()
            .map(|&(is_node, id)| {
                if is_node {
                    PathValue::Node(id)
                } else {
                    PathValue::EdgeDirectional(id)
                }
            })
            .collect();
        ResultRow::new(Path(pvs), Assignment::new())
    }

    fn n(id: Id) -> (bool, Id) {
        (true, id)
    }
    fn e(id: Id) -> (bool, Id) {
        (false, id)
    }

    #[test]
    fn trail_rejects_repeated_edge() {
        // 1 -e10-> 2 -e10-> 3 reuses edge 10.
        let row = path_row(&[n(1), e(10), n(2), e(10), n(3)]);
        assert!(!path_satisfies_mode(row.path(), PathMode::Trail));
        // distinct edges are fine
        let ok = path_row(&[n(1), e(10), n(2), e(11), n(3)]);
        assert!(path_satisfies_mode(ok.path(), PathMode::Trail));
    }

    #[test]
    fn acyclic_rejects_any_repeated_node() {
        // 1 -> 2 -> 1 repeats node 1.
        let row = path_row(&[n(1), e(10), n(2), e(11), n(1)]);
        assert!(!path_satisfies_mode(row.path(), PathMode::Acyclic));
    }

    #[test]
    fn simple_allows_only_first_equals_last() {
        // closed cycle 1 -> 2 -> 1 is SIMPLE but not ACYCLIC
        let cycle = path_row(&[n(1), e(10), n(2), e(11), n(1)]);
        assert!(path_satisfies_mode(cycle.path(), PathMode::Simple));
        assert!(!path_satisfies_mode(cycle.path(), PathMode::Acyclic));
        // an interior repeat (1 -> 2 -> 1 -> 3) is not SIMPLE
        let bad = path_row(&[n(1), e(10), n(2), e(11), n(1), e(12), n(3)]);
        assert!(!path_satisfies_mode(bad.path(), PathMode::Simple));
    }

    #[test]
    fn shortest_paths_keeps_min_length_per_boundary() {
        // Two paths 1..9: one of length 2, one of length 3.
        let short = path_row(&[n(1), e(10), n(5), e(11), n(9)]);
        let long = path_row(&[n(1), e(20), n(6), e(21), n(7), e(22), n(9)]);
        let mut rows = vec![long.clone(), short.clone()];
        select_shortest_paths(&mut rows, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(row_length(&rows[0]), 2);
    }

    #[test]
    fn shortest_groups_keeps_ties() {
        // Two length-2 paths and one length-3 path, same boundary (1,9).
        let a = path_row(&[n(1), e(10), n(5), e(11), n(9)]);
        let b = path_row(&[n(1), e(12), n(6), e(13), n(9)]);
        let c = path_row(&[n(1), e(20), n(7), e(21), n(8), e(22), n(9)]);
        let mut rows = vec![a, b, c];
        select_shortest_groups(&mut rows, 1);
        // both length-2 paths survive, the length-3 one is dropped
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| row_length(r) == 2));
    }

    #[test]
    fn any_caps_per_boundary() {
        let a = path_row(&[n(1), e(10), n(9)]);
        let b = path_row(&[n(1), e(11), n(9)]);
        let c = path_row(&[n(1), e(12), n(9)]);
        // different boundary
        let d = path_row(&[n(2), e(13), n(9)]);
        let mut rows = vec![a, b, c, d];
        select_any(&mut rows, 2);
        // 2 from (1,9) + 1 from (2,9)
        assert_eq!(rows.len(), 3);
    }
}
