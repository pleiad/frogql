# Implementation Plan: Full Factorized Query Execution (Complete Rewrite)

This plan details a **complete rewrite** of the intermediate join execution layer in `frogql` to support **Full Factorized Execution**. It addresses the recent regression in path-repetition tests by introducing a formal distinction between endpoint-based path concats and variable-based Cartesian/natural joins.

---

## Technical Context & Architectural Gaps

In a query engine using factorized execution, relations are kept in a compressed representation (a tree or forest of independent factors) to avoid the combinatorial explosion of flat cross-products.

Previously, the hybrid approach attempted to merge all product operations under a single `FactorNode::Product` variant. However, query execution performs two semantically distinct kinds of products:

1. **Endpoint-based Path Concatenation**:
   - **Context**: Concatenating path steps (e.g. `(a)-[e1]->(b)-[e2]->(c)` or repetitions `-->{1,2}`).
   - **Logic**: Join on the endpoint node IDs (`last_node_id(left) == first_node_id(right)`) and extend the path using `r1.extend_path(r2.path())`.
   - **Key**: This must NOT treat variables or assignments as the primary join key, but rather the path's structural endpoints.

2. **Variable-based Join (Natural/Disjoint)**:
   - **Context**: Joining separate query branches (e.g. `MATCH (a)-[:knows]->(b), (a)-[:likes]->(c)`).
   - **Logic**: Join on shared variable bindings in the assignment table (or cross-product if disjoint), concatenating independent paths via `ResultRow::join(r1, r2)`.
   - **Key**: This must NOT unify paths along node endpoints.

By conflating these two under a single `FactorNode::Product` variant, variable-based natural joins were incorrectly attempting path-endpoint lookups, leading to empty results and test failures (e.g. `-->{1,2}` returning 12 instead of 23 rows).

---

## Proposed Architecture

### 1. Differentiated FactorNode Variants
We will redefine `FactorNode` in [result.rs](file:///home/maxdemarzi/frogql/src/runtime/result.rs) to represent both join semantics:

```rust
#[derive(Debug, Clone)]
pub enum FactorNode {
    Flat(Vec<ResultRow>),
    /// Variable-based Cartesian / Natural Join (joins assignments, concatenates path lists).
    Product(Vec<FactorNode>),
    /// Endpoint-based Path Concatenation (joins endpoints, extends the last path).
    PathConcat(Vec<FactorNode>),
    Union(Vec<FactorNode>),
}
```

### 2. Recursive Factorized Partitioning & Natural Join
To implement full factorized execution without flat-materializing natural joins on shared keys, we will implement recursive partitioning directly on `FactorNode`:

- **Free Variables**: A helper `FactorNode::freevars()` determines which variables are bound within a factor.
- **Partitioning**: `FactorNode::partition(self, x: &str)` splits a factorized tree recursively into a `HashMap<PathValue, FactorNode>` based on variable `x`.
  - For `Product(sub)` or `PathConcat(sub)`, it recursively descends only into the sub-node that binds `x`, leaving all independent factors untouched.
- **Natural Join**: To join `left` and `right` on shared key `x`, we partition both sides by `x` and build a union of products:
  ```rust
  FactorNode::Union(
      common_keys.map(|k| FactorNode::Product(vec![left_partition[k], right_partition[k]]))
  )
  ```
  This preserves the factorized layout of all other non-shared variables.

### 3. Comprehensive Lazy Flattening Boundaries
To maintain backward compatibility with external bindings (Python, Node, WASM), tests, sorting, and projection, all query boundaries will lazily invoke `IntermediateResult::ensure_flat()` before reading or mutating `.rows`.

---

## Proposed Changes

### Core Result Types

#### [MODIFY] [result.rs](file:///home/maxdemarzi/frogql/src/runtime/result.rs)
- Update `FactorNode` enum to support `Product` (variable-based) and `PathConcat` (endpoint-based) variants.
- Implement `FactorNode::flatten()`, dispatching to `join_flat_rows_variable` and `join_flat_rows_endpoint` respectively.
- Implement `FactorNode::freevars()` and `FactorNode::partition(self, x: &str)`.
- Implement `FactorNode::join(self, other: FactorNode, join_var: &str)` using partition-based alignment.

---

### Query Engine

#### [MODIFY] [engine.rs](file:///home/maxdemarzi/frogql/src/runtime/engine.rs)
- Update `natural_join` and `left_outer_join` to accept owned `IntermediateResult` parameters and call `.ensure_flat()` at their start.
- Update `run_match_chain` to pass owned parameters to `natural_join` and `left_outer_join`, and accept mutable `acc` for optional pushdowns.
- Update `optional_via_bind_pushdown` to take `acc: &mut IntermediateResult` and call `acc.ensure_flat()`.
- Update `run_concat_pattern` to construct `FactorNode::PathConcat` for fallbacks, and call `ir1.ensure_flat()` before optimized concats.
- Call `ensure_flat()` on intermediate results at all query boundaries:
  - Inside `run_path_pattern` for `Union`, `Filter`, `Selected`, and `Named` arms.
  - Inside `run_join` before hash-indexing or cross-product loops.
  - At the end of public entry points: `run`, `run_with_limit`, and `run_query`.

---

### DML Boundary

#### [MODIFY] [dm.rs](file:///home/maxdemarzi/frogql/src/runtime/dm.rs)
- Call `ir.ensure_flat()` in `run_dm` after evaluating the MATCH prefix to ensure DML inserts/deletes have flat row access.

---

## Verification Plan

### Automated Tests
- Run the workspace test suite sequentially to verify that all 192 tests pass successfully:
  ```bash
  cargo test --workspace -- --test-threads=1
  ```
- Run clippy to ensure no warnings or lint errors:
  ```bash
  cargo clippy --workspace --all-targets -- -D clippy::all
  ```
