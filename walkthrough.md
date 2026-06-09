# Walkthrough: Full Factorized Query Execution

We have successfully completed a **complete rewrite** of the intermediate join and execution layer in `frogql` to support **Full Factorized Execution**. 

This execution model allows intermediate relations to remain in a compressed tree representation (using products and unions of disjoint factors) rather than materializing combinatorial Cartesian products of query branches.

---

## Changes Implemented

### 1. Factorized Results & Forest Model
- Redefined `FactorNode` in [result.rs](file:///home/maxdemarzi/frogql/src/runtime/result.rs) with:
  - `Flat(Vec<ResultRow>)` for leaf relations.
  - `Product(Vec<FactorNode>)` for variable-based Cartesian/natural joins.
  - `PathConcat(Vec<FactorNode>)` for path step/endpoint concatenations.
  - `Union(Vec<FactorNode>)` for query branches/alternatives.
- Added `FactorNode::is_empty()` and `IntermediateResult::is_empty()` to check for empty factorized paths without flattening.
- Enhanced `FactorNode::Union`'s flattening logic to lazily pad assignments with `Nothing` according to GQL Union semantics using `fill_nones(&dom)`.

### 2. Recursive Factorized Partitioning & Join Algorithms
- Implemented `FactorNode::freevars()` to track bound variables in factorized trees.
- Implemented recursive `FactorNode::partition(self, x: &str)` to split factorized representations based on variable values.
- Implemented `FactorNode::join(self, other: FactorNode, join_var: &str)` to align partitioned left/right factor nodes recursively, producing a factorized union of products without Cartesian materialization.

### 3. Factorized Join and Concat Operators
- Refactored `natural_join` in [engine.rs](file:///home/maxdemarzi/frogql/src/runtime/engine.rs) to execute fully factorized variable-based joins without early flattening.
- Refactored `left_outer_join` in [engine.rs](file:///home/maxdemarzi/frogql/src/runtime/engine.rs) to partition on the join variable and perform factorized padded joins, propagating `Nothing` values safely using lazy flat leaf placeholders.
- Simplified `run_join` in [engine.rs](file:///home/maxdemarzi/frogql/src/runtime/engine.rs) to delegate fallback pairwise joins directly to `natural_join`, completely removing manual flat hash-join loops.
- Defer `ensure_flat()` in `run_concat_pattern` to only when optimized adjacency traversal is chosen. Fallback path concatenations construct `FactorNode::PathConcat` directly from factorized sub-results.
- Updated `PathPattern::Union` execution in [engine.rs](file:///home/maxdemarzi/frogql/src/runtime/engine.rs) to perform lazy factorized unions unless a limit is active, in which case it flattens as needed.

### 4. Zero-Regression Boundary Flattening
- Retained backward-compatibility for external bindings (Python, Node, WASM) and tests by invoking `.ensure_flat()` at query boundaries (e.g. `run`, `run_with_limit`, `run_query`, filters, and sorting).

---

## Verification Results

### Automated Tests
- Ran the entire test suite sequentially:
  ```bash
  cargo test --workspace -- --test-threads=1
  ```
  **Result**: 193 passed (0 failed, 1 ignored). All tests, including the path repetition test suite (`unused_variable_repetition_test`), pass successfully.

### Clippy Cleanliness
- Ran clippy on the workspace:
  ```bash
  cargo clippy --workspace --all-targets -- -D clippy::all
  ```
  **Result**: Compiles and finishes clean with no warnings or errors.
