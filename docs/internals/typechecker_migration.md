# Typechecker migration — Phase 1 mapping & decisions

This document is the working record of porting the typechecker from `fppc`
onto gqlite. It is written before any checker code lands and is updated
through the migration. The source spec is `gqlite_migration_phase1_as_is.md`
in the parent directory; that document is canonical for *what* this phase is.
This document captures *how* the port is being executed against gqlite as
it actually exists.

## PathType decision (option 1)

Per the source spec, **option 1** is chosen: port `fppc`'s `PathType` onto
gqlite as a parallel module at `src/typing/path_type.rs`. Rationale, verbatim
from the spec ("Decision: option 1 (port `PathType` as-is)"):

> Chosen rationale: keep a functional implementation at every step of the
> migration. The port already carries several sources of risk — new AST,
> new lattice variants (`SimpleType::F`, `SimpleType::Record`), new pipeline
> integration. Reshaping `PathType` at the same time compounds those risks
> and delays the moment at which the checker is end-to-end testable against
> fppc's behavior. Option 1 preserves the fppc design 1:1, which means: the
> checker can be validated against fppc's existing test suite and manual
> queries before any refactor is attempted; each later change
> (canonicalization, `PathType` unification, interning) lands on top of a
> known-good baseline; the design debt that options 2 and 3 address is
> real, but it is refactorable once the port works.

Path-shape and variable-type concerns stay separate. **`VariableType::Group`
in gqlite is repetition grouping (from `{n,m}` quantifiers) — it is NOT a
path type.** The path-shape information lives in `PathType`, not in
`VariableType::Group`.

## User override (2026-04-26): fppc is authority on type-checking semantics

The source spec says "phase 1 does not fix the gqlite-side bugs". The user
overrides this rule for **type-checking semantics**: where a gqlite lattice
op differs from fppc on a case fppc covers, gqlite is updated to match fppc
in this phase. Cases gqlite covers but fppc does not (`LabelType::{Top, Empty,
Neg}`, `SimpleType::F`, `SimpleType::Record`, `SimpleType::Group`,
`VariableType::Group`, `EdgeNonDirectional`, `EdgeAnyDirection`) are
gqlite-only and stay as-is. Fppc's *own* preserved-by-design behaviors (the
`(Or-LHS, _)` shadowing in `LabelType::is_subtype`, etc.) are ported
verbatim.

This means the lattice reconciliation work happens **before** the checker
port (Step 1), not as a follow-up.

## AST mapping

| fppc | gqlite | note |
|---|---|---|
| `ast::Expr::Constant(Constant)` | `syntax::expr::Expr::Const(model::value::Value)` | different value type; map `Int↔Z`, `Float↔F`, `Str↔S`, `Bool↔B`. fppc has only Int/String/Bool; gqlite adds Float and List/Record (gqlite-only) |
| `ast::Expr::TypeLiteral(SimpleType)` | `syntax::expr::Expr::Type(SimpleType)` | renamed |
| `ast::Expr::AttributeLookup(Var, Var)` | `syntax::expr::Expr::AttrLookup { var: String, attr: String }` | gqlite uses raw `String`; entity `.0` access becomes the `var` field |
| `ast::Expr::Binop(BinOpKind, Box, Box)` | `syntax::expr::Expr::Binop { op: BinOp, left: Box, right: Box }` | reuse `BinOp::delta` from `syntax::expr` |
| `ast::Expr::Unop(UnOpKind, Box)` | `syntax::expr::Expr::Unop { op: UnOp, operand: Box }` | reuse `UnOp::delta` |
| — | `Expr::FieldAccess { base, field }` | gqlite-only; record-field access on `SimpleType::Record`. Out of fppc scope. |
| `ast::PathPattern::Node(Option<Descriptor>)` | `syntax::path_pattern::PathPattern::Node(Option<Descriptor>)` | same |
| `ast::PathPattern::Edge(EdgeDirection, Option<Descriptor>)` | split: `EdgeRight`, `EdgeLeft`, `EdgeUndirected`, `EdgeAnyDirection` | dispatch on direction at the match site. fppc has `EdgeDirection::{Right, Left, Any, None}`; gqlite has four enum variants. `EdgeAnyDirection` (gqlite) maps to fppc `EdgeDirection::Any`; `EdgeUndirected` (gqlite) maps to fppc `EdgeDirection::None` |
| `ast::PathPattern::Concat(Box, Box)` | `PathPattern::Concat(Box, Box)` | same |
| `ast::PathPattern::Union(Box, Box)` | `PathPattern::Union(Box, Box)` | same |
| `ast::PathPattern::Filter(Box, Expr)` | `PathPattern::Filter(Box, Expr)` | same; gqlite elaboration drains `Descriptor::value_filters` into `Filter` nodes before typecheck runs |
| `ast::PathPattern::Quantified(Box, Quantifier)` | `PathPattern::Repeat { pattern: Box, lb: usize, ub: Option<usize> }` | gqlite has explicit lb/ub instead of a `Quantifier` enum |
| `ast::PathPattern::Questioned(Box)` | `PathPattern::Questioned(Box)` | same |
| — | `PathPattern::Join(Box, Box)` | gqlite-only; comma-separated patterns. Treat as `Concat` for typing (compose two patterns sharing endpoint variables); flagged for review at Step 6 |
| `ast::Descriptor { variable: Option<Var>, descriptor_type: DescriptorType }` | `syntax::descriptor::Descriptor { var: Option<String>, dtype: DescriptorType, value_filters: Vec<(String, Expr)> }` | `value_filters` should be empty post-elaboration; assert it. `var` is `String` not `Var`. |
| `ast::BinOpKind`, `ast::UnOpKind` | `syntax::expr::BinOp`, `syntax::expr::UnOp` | use their `delta()` |
| `ast::EdgeDirection::{Right, Left, Any, None}` | inferred from gqlite edge variant at match site | no enum needed in checker; the variant of `PathPattern` already encodes direction |
| `ast::Var` | `String` | gqlite uses raw strings throughout |

## Lattice mapping

| fppc | gqlite | status |
|---|---|---|
| `ast::SimpleType::{Base(Int/Bool/String), Star, Union, List, Zero}` | `typing::simple_type::SimpleType::{Z, B, S, F, Star, Zero, Union, Group, List, Record}` | fppc atoms are nested under `Base(BaseType)`; gqlite has flat atoms. fppc has no `F`/`Group`/`Record`. Gqlite's `Group` is the moral equivalent of fppc's `List` for typechecker-internal repetition grouping; gqlite reserves `List` for user-facing list values. |
| `ast::LabelType::{Label, Star, And, Or}` | `typing::label_type::LabelType::{Label, Star, Top, Empty, And, Or, Neg}` | gqlite-only: `Top`, `Empty`, `Neg` |
| — | `typing::property_type::PropertyType::{Open, Closed, Zero}` | fppc has a `PropertyType` with at least `Closed` (per fppc tests); gqlite adds `Open`. The `Open`/`Closed` distinction is gqlite-only. `Closed × Closed` is the case fppc covers. |
| `ast::DescriptorType { label, properties }` | `typing::descriptor_type::DescriptorType { label, props }` | renamed field; same shape |
| `typechecker::variable_type::{NodeType, EdgeType, EdgeKind, VariableType::{Node, Edge, Union, List, Zero}}` | `typing::variable_type::VariableType::{Node, EdgeDirectional, EdgeNonDirectional, Union, Group, Zero}` | structural divergence. fppc has separate `NodeType(DescriptorType)` and `EdgeType { descriptor, left, right, kind }` types; gqlite inlines them. fppc's single `Edge(EdgeType)` with `kind: EdgeKind` splits into gqlite's two enum variants. fppc's `List` ↔ gqlite's `Group` for repetition grouping. |
| `typechecker::path_type::{PathType, NodePathType, EdgePathType}` | (none) | absent in gqlite — port as `src/typing/path_type.rs` per option 1 |
| `typechecker::schema::Schema { nodes: Vec<NodeType>, edges: Vec<EdgeType> }` | `typing::variable_type::Schema { nodes: Vec<VariableType>, edges: Vec<VariableType> }` | gqlite stores schema entries as `VariableType` (always `Node(_)` or `EdgeDirectional/EdgeNonDirectional`). Both have `star()`; fppc adds a `new(nodes, edges)` constructor — gqlite's public fields make a struct literal sufficient. |
| `typechecker::type_environment::TypeEnvironment` | (none) | port as `src/typing/type_environment.rs` |

## Lattice reconciliation table

For each lattice op the checker calls, this table records fppc's behavior,
gqlite's current behavior on the same case, whether they match, and the
planned change. **Step 1** of the migration applies these changes.

`Match? = Y` means no change. `Match? = N` means gqlite is updated to fppc.
Any row marked `gqlite-only` is for cases fppc doesn't cover; it stays
as-is and is not subject to reconciliation.

### `SimpleType`

| Op | Case | fppc | gqlite | Match? | Change |
|---|---|---|---|---|---|
| `union` | `(Zero, _)` / `(_, Zero)` | other side | other side | Y | — |
| `union` | `a == b` | `a` | `a` | Y | — |
| `union` | else | `Union(a, b)` | `Union(a, b)` | Y | — |
| `meet` | `(Star, _)` / `(_, Star)` | other side | other side | Y | — |
| `meet` | `(Union(t1, t2), _)` | `union(meet(t1,b), meet(t2,b))` | same | Y | — |
| `meet` | `(_, Union(t1, t2))` | symmetric | same | Y | — |
| `meet` | `a == b` | `a` | `a` | Y | — |
| `meet` | else | `Zero` | `Zero` | Y | — |
| `is_subtype` | `(Star, _)` / `(_, Star)` | `true` | `true` | Y | — |
| `is_subtype` | bottom-arm | `(Zero, _) => true` | **`(_, Zero) => true`** | **N** | gqlite bug B2: change `(_, Zero)` to `(Zero, _)`. Bottom is subtype of everything, not vice versa. |
| `is_subtype` | `(Union(a,b), _)` | `is_subtype(a,t2) \|\| is_subtype(b,t2)` | same | Y | — |
| `is_subtype` | `(_, Union(a,b))` | `is_subtype(t1,a) \|\| is_subtype(t1,b)` | same | Y | — |
| `is_subtype` | `(List, List)` covariance | `_ => t1==t2` (no special arm; meets only same-inner) | `(List(a),List(b)) => is_subtype(a,b)` | gqlite-only enrichment | keep gqlite's covariance — fppc's lack is a gqlite-only improvement, not a divergence to fix |
| `is_subtype` | `(Group,Group)`, `(Record,Record)` | n/a | gqlite-specific | gqlite-only | keep |
| `is_empty` | `Zero` | `true` | `true` | Y | — |
| `is_empty` | `Union(a,b)` | `is_empty(a) && is_empty(b)` | same | Y | — |
| `is_empty` | `List(t)` | `is_empty(t)` | same | Y | — |
| `is_empty` | other gqlite variants (`Group`, `Record`) | n/a | gqlite-specific | gqlite-only | keep |

### `LabelType`

| Op | Case | fppc | gqlite | Match? | Change |
|---|---|---|---|---|---|
| `meet` | `(Star, _)` / `(_, Star)` | other side | other side | Y | — |
| `meet` | `(Top, _)` / `(_, Top)` | n/a | other side | gqlite-only | keep |
| `meet` | `is_subtype(a,b)` | `a` | `a` | Y | — |
| `meet` | `is_subtype(b,a)` | `b` | `b` | Y | — |
| `meet` | else | `And(a,b)` | `And(a,b)` | Y | — |
| `is_subtype` | `(Star, _)` / `(_, Star)` | `true` | `true` | Y | — |
| `is_subtype` | `(_, Top)` | n/a | `true` | gqlite-only | keep |
| `is_subtype` | `(Label, Label)` | `a == b` | `a == b` | Y | — |
| `is_subtype` | `(_, And(a,b))` | `is_subtype(l1,a) && is_subtype(l1,b)` | same | Y | — |
| `is_subtype` | `(_, Or(a,b))` | `is_subtype(l1,a) \|\| is_subtype(l1,b)` | same | Y | — |
| `is_subtype` | `(And(a,b), _)` | `is_subtype(a,l2) \|\| is_subtype(b,l2)` (gradual) | same | Y | — |
| `is_subtype` | `(Or(a,b), _)` | `is_subtype(a,l2) \|\| is_subtype(b,l2)` (fppc-preserved bug: should be `&&`, but fppc uses `\|\|`) | **arm missing** | **N** | add `(Or(a,b), _) => is_subtype(a,l2) \|\| is_subtype(b,l2)` after the `(And, _)` arm. Port fppc's `\|\|` verbatim — fppc preserves this by design, so we do too. |
| `is_subtype` | `(_, Neg(inner))` | n/a | `!is_subtype(l1, inner)` | gqlite-only | keep |
| `is_subtype` | `(Empty, _)` | n/a | `true` | gqlite-only | keep |
| `is_subtype` | else | `false` (implicit; fppc's match is exhaustive over its variants) | `false` | Y | — |
| `is_empty` | any | `false` (not in fppc's typechecker; trivial) | always `false` | Y (compatible) | — |

### `PropertyType`

fppc has a `PropertyType` with at least `Closed`; the open/closed distinction
is gqlite-only. `Closed × Closed` is the only fully fppc-covered case.

| Op | Case | fppc | gqlite | Match? | Change |
|---|---|---|---|---|---|
| `meet` | `Closed × Closed` (same keys) | per-field meet | per-field meet | Y | — |
| `meet` | `Closed × Closed` (different keys) | (assumed) `Zero` | `Zero` | Y | — |
| `meet` | involves `Open` | n/a | gqlite-specific | gqlite-only | keep |
| `is_subtype` | `Closed × Closed` | same keys, per-field is_subtype | same keys, per-field is_subtype | Y | — |
| `is_subtype` | involves `Open` | n/a | gqlite-specific | gqlite-only | keep |
| `is_empty` | `Closed` with empty bottom field | depends on field types | covered | Y | — |
| `is_empty` | `Open` / `Zero` | n/a | gqlite-specific | gqlite-only | keep |

No reconciliation changes for `PropertyType`.

### `DescriptorType`

| Op | Case | fppc | gqlite | Match? | Change |
|---|---|---|---|---|---|
| `meet` | conjunction of label and props meets | same | same | Y | — |
| `is_subtype` | conjunction of label and prop subtype | same | same | Y | — |
| `is_empty` | label or props empty | label or props empty | Y | — |

No reconciliation changes for `DescriptorType`. Downstream behavior changes
through its dependencies (`LabelType::is_subtype` after Step 1).

### `VariableType`

| Op | Case | fppc | gqlite | Match? | Change |
|---|---|---|---|---|---|
| `meet` | `(Zero, _)` / `(_, Zero)` | `Zero` | `Zero` (catch-all) | Y | — |
| `meet` | `(List, List)` / `(Group, Group)` | recurse on inner | recurse on inner | Y | — (gqlite uses Group as the equivalent name) |
| `meet` | `(Node, Node)` | meet descriptors | meet descriptors | Y | — |
| `meet` | `(Edge, Edge)` directed×directed | meet desc + meet endpoints | same on `EdgeDirectional × EdgeDirectional` | Y | — |
| `meet` | `(Edge, Edge)` undirected×undirected | meet both orientations, join | same on `EdgeNonDirectional × EdgeNonDirectional` | Y | — |
| `meet` | `(Edge, Edge)` directed×undirected | `Err(...)` (errors propagate) | falls through to `_ => Zero` | divergent but compatible | accept; the checker treats `Zero`/empty as the failure signal where fppc would surface `Err`. The signature difference (fppc returns `Result`, gqlite returns `VariableType`) is gqlite-only and intentional. |
| `meet` | `(Union(t1,t2), _)` | per-side meet, then `join(v1, v2)` (Err handling absorbs failures into the other side) | per-side meet, then manual `Union(r1, r2)` (Zero handling discards bottoms) | **N** | replace the manual `Union(r1, r2)` construction with `VariableType::join(&r1, &r2)` so equal sides dedupe and Zero is absorbed — matches fppc's `join`-based combination |
| `meet` | `(_, Union(_,_))` | symmetric | symmetric | Y | (cascades from above change) |
| `meet` | mismatched shapes | `Err(...)` | `Zero` | divergent (signature) | accept as above |
| `is_subtype` | `(Zero, _)` | `true` | **arm missing** | **N** | add `(VariableType::Zero, _) => true` arm at the top |
| `is_subtype` | `(Node, Node)` | descriptor subtype | descriptor subtype | Y | — |
| `is_subtype` | `(EdgeDirectional, EdgeDirectional)` | descriptor + endpoints subtype | same | Y | — |
| `is_subtype` | `(EdgeNonDirectional, EdgeNonDirectional)` | symmetric on directed projections | same | Y | — |
| `is_subtype` | `(List, List)` / `(Group, Group)` | recurse | **arm missing** for `Group` | **N** | add `(Group(a), Group(b)) => is_subtype(a, b)` |
| `is_subtype` | `(Union(a,b), _)` | `is_subtype(a,t2) \|\| is_subtype(b,t2)` | **arm missing** | **N** | add |
| `is_subtype` | `(_, Union(a,b))` | `is_subtype(t1,a) \|\| is_subtype(t1,b)` | **arm missing** | **N** | add |
| `is_subtype` | else | `false` | `false` | Y | — |
| `join` | `(Zero, _)` / `(_, Zero)` / `a==b` / else | identical | identical | Y | — |
| `join_from_list` | empty | `Zero` | `Zero` | Y | — |
| `join_from_list` | non-empty | left fold with `join` | left fold with `join` | Y | — |
| `refine` | `Node` | iterate schema.nodes, filter by `is_subtype`, meet, join | same | Y | (downstream-fixed by `is_subtype` change) |
| `refine` | `Edge*` | analogous | analogous | Y | — |
| `refine` | `Union` | recurse and join | recurse and join | Y | — |
| `refine` | `List` / `Group` | recurse, wrap | recurse, wrap | Y | (gqlite uses Group as the equivalent name) |
| `refine` | `Zero` | `Zero` | `Zero` | Y | — |
| `is_empty` | all variants | as in source | as in source | Y | — |

**Helper:** fppc's `VariableType::refine_to_nodes(schema, &t) -> Vec<NodeType>`
is used by `PathType::meet` to enumerate concrete schema nodes after a
descriptor narrowing. gqlite has no analog. **Add** as
`VariableType::refine_to_nodes(schema, &t) -> Vec<VariableType>` returning
the `Node(_)` variants reachable after refinement. Used only by `PathType`
in this phase.

### Summary of Step 1 changes

1. `src/typing/simple_type.rs`: flip `(_, Zero) => true` to
   `(Zero, _) => true` in `is_subtype`.
2. `src/typing/label_type.rs`: insert `(Or(a,b), _) => is_subtype(a, l2) || is_subtype(b, l2)`
   arm in `is_subtype`, after the `(And, _)` arm. Use `||` (matches fppc's
   preserved-by-design behavior).
3. `src/typing/variable_type.rs`:
   - Add `(VariableType::Zero, _) => true` arm to `is_subtype` (top of match).
   - Add `(Group(a), Group(b)) => is_subtype(a, b)` arm to `is_subtype`.
   - Add `(Union(t1, t2), _) => is_subtype(t1, t2_outer) || is_subtype(t2, t2_outer)` arm.
   - Add `(_, Union(t1, t2))` symmetric arm.
   - In `meet`, replace the explicit Union-LHS `Union(r1, r2)` construction
     with `VariableType::join(&r1, &r2)` so equal sides dedupe.
   - Add `pub fn refine_to_nodes(schema: &Schema, t: &VariableType) -> Vec<VariableType>`.
4. Tests in `src/typing/` that asserted any of the pre-fppc behaviors are
   updated; this is expected, not a regression.

## fppc Schema vs gqlite Schema

fppc's `Schema { nodes: Vec<NodeType>, edges: Vec<EdgeType> }` and gqlite's
`Schema { nodes: Vec<VariableType>, edges: Vec<VariableType> }` are
isomorphic for entries, since gqlite always stores `Node(_)` /
`EdgeDirectional` / `EdgeNonDirectional` `VariableType` values. The checker
threads `&Schema` through `refine` and `PathType::meet` — the same shape
fppc uses.

gqlite's `Schema` does not derive `Clone` or `Debug`. The checker stores a
`Schema` by value (mirroring fppc's `Typechecker { schema: Schema, ... }`),
so the Step 1 commit also adds `#[derive(Debug, Clone)]` on `Schema`.

## Checker entry-point shape (planned)

The checker's public surface mirrors fppc's:

```rust
pub struct Typechecker {
    pub schema: Schema,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub struct TypecheckResult {
    pub path: PathType,
    pub env: TypeEnvironment,
    pub ok: bool,
    pub empty: bool,
}

impl Typechecker {
    pub fn new(schema: Schema) -> Self;
    pub fn untyped() -> Self;                         // Schema::star()
    pub fn check_query(&mut self, q: &Query) -> TypecheckResult;
    pub fn check_pattern(&mut self, p: &PathPattern) -> TypecheckResult;
    fn check_path_pattern(&mut self, p: &PathPattern) -> TypecheckResult;
    fn check_expr(&mut self, e: &Expr, env: &TypeEnvironment) -> SimpleType;
    fn refine_pattern_node(&self, desc: &Option<Descriptor>) -> VariableType;
    fn refine_pattern_edge(&self, dir: EdgeDir, desc: &Option<Descriptor>) -> VariableType;
    fn pow_path_type(&self, p: &PathType, n: u64) -> PathType;
}
```

The `EdgeDir` argument used by `refine_pattern_edge` and `PathType::from`
is a small typechecker-local enum mirroring fppc's `EdgeDirection::{Right,
Left, Any, None}` — defined alongside `PathType` since gqlite has no AST
counterpart.

## Pipeline integration (planned, Step 7)

Insert between `elaborate` and `optimizer::compile` in `src/lib.rs`:

```rust
pub fn compile_query(input: &str) -> Result<Query, String> {
    let q = parser::parse_query(input)?;
    let q = elaborate::elaborate_query(q);
    let mut tc = typing::Typechecker::untyped();
    let r = tc.check_query(&q);
    if !r.ok {
        return Err(tc.errors.join("; "));
    }
    let optimized_pattern = optimizer::compile(q.pattern);
    Ok(Query { pattern: optimized_pattern, ..q })
}
```

Sibling entry points `compile_unchecked` / `compile_query_unchecked` skip
the typecheck step and produce the same plan as gqlite/main does today.
The default schema is `Schema::star()` (permissive); a real schema source
is a later phase.

## Punts (recorded for later phases)

- `Expr::FieldAccess` on non-record receivers — minimal record-only typing
  in this phase; non-record cases use `todo!()` and are flagged here when
  the checker lands.
- gqlite-only list typing (`SimpleType::List`) at expression positions —
  not exercised by fppc, deferred.
- A `NodeId → Type` side-table for the optimizer.
- Schema sourcing — only `Schema::star()` for now.
- Refactoring `PathType` away (options 2 and 3 from the source spec).

## Architectural follow-ups carried forward by option 1

- Parallel typing modules (`path_type.rs` and `variable_type.rs` carry
  duplicate node info per edge position).
- Per-segment clones at `Concat` / `Union` (fppc's data movement profile).

These costs are accepted for phase 1 and not addressed here.

---

# Final report

This section is appended at the end of phase 1 and reflects what the
migration actually shipped, vs. what the spec and the Step 0 plan
called for. Each entry is a deviation, a punt, or a finding — anything
that should be visible to whoever reads this next.

## Commits on `typing/checker`

```
082d17e  Add typechecker migration mapping doc          (Step 0)
7a50284  Reconcile lattice ops with fppc                (Step 1)
02a648d  Port TypeEnvironment from fppc                 (Step 2)
674a8c5  Port PathType and stub checker module          (Step 3)
be16449  Implement check_expr                           (Step 4)
681a0ed  Implement refine_pattern_node and refine_pattern_edge (Step 5)
76ef1bd  Implement check_path_pattern                   (Step 6)
26de14a  Wire typechecker into compile and compile_query (Step 7)
a1f47f3  Add typechecker smoke tests                    (Step 8)
```

The branch is intended as the long-lived integration branch for typecheck
work; phase-2 changes land here too.

## Outcome vs. acceptance criteria

| Criterion (from the source spec) | Status |
|---|---|
| `typechecker_migration.md` complete and accurate | done — this file |
| `src/typing/checker.rs` and `src/typing/type_environment.rs` exist, compile, expose entry points | done; also `path_type.rs` per option 1 |
| `src/lib.rs` wires the checker between elaborate and optimize, surfacing errors as `String`. On by default | done — `Typechecker::untyped()` (Schema::star) is the default |
| Opt-out path exists | done — `compile_unchecked` / `compile_query_unchecked` |
| `cargo build` clean (no new warnings beyond baseline) | done — 4 pre-existing warnings in `bin/gqlite` only |
| Smoke queries exercise the checked and opt-out paths | done — `tests/typecheck_smoke.rs`, 4 tests pass |
| Final report covers deviations, punts, and gqlite-side findings | this section |

Test totals on `typing/checker`:
- `cargo test --lib`: 62 passing (same as `main`)
- `cargo test --test typecheck_smoke`: 4 passing (new — wire-level smoke)
- `cargo test --test typecheck_test`: 45 passing (new — direct
  translation of fppc's typechecker test suite; see below)
- All other integration tests still pass with no modification — 254 total
- `cargo test --test bench_test`: 2 of 4 failing with `attempt to subtract
  with overflow` — **identical failure mode to `main`**, pre-existing per
  CLAUDE.md, unrelated to this branch
- `cargo run --release --example typecheck_repl_smoke` runs four
  representative queries against `examples/movies.gdb` end-to-end through
  `compile_query` and a fifth (unbound-variable) query that's correctly
  rejected by the checked path and accepted by the unchecked path. All
  five pass; the runtime returns expected row counts (38 movies, 133
  people, etc.)

## Equivalence with fppc: 45/46 tests translated

`tests/typecheck_test.rs` translates fppc's typechecker test suite in
`fppc/src/typechecker/checker.rs::tests` line-for-line into gqlite. Each
test mirrors a fppc test with the same name and asserts the same `ok` /
`empty` / warning predicates. Surface-syntax differences (`{a: int}` →
`{a is int}`, bare `->` → `()-[]->()` for parser anchoring) are adapted
in the query string; the typechecker behavior under test is unchanged.

All 45 pass on first run. The single skip is `test_incompatible_records`
(`(x: {{a: bool}})(x: {b: int})`) — gqlite's parser treats double-brace
nested record syntax differently and the equivalent surface query isn't
trivially expressible. The lattice case it exercises (Record × Record
with disjoint fields collapses) is covered by the closed-schema
`test_example23` / `test_example24` and `test_example22` paths.

This closes the validation gap noted in Step 9. The remaining 0.5 of
divergence (vs full equivalence) is the cross-direction-edge-meet error
path documented under "Deviations from the Step-0 plan" §2 — fppc errors,
gqlite produces `Zero`. None of the 45 ported tests exercise this
specific path, so observable behavior on fppc's surface area matches.

## Lattice reconciliation outcomes

Step 1 applied the changes the Step-0 reconciliation table called out, no
more and no less. For each touched op, the citation is `fppc/<file>:<line>`;
the gqlite location is `src/typing/<file>:<line>`.

### `SimpleType::is_subtype`

- **fppc:** `src/ast/types.rs:46–58`. The bottom arm is
  `(SimpleType::Zero, _) => true`.
- **gqlite before:** `src/typing/simple_type.rs:66`,
  `(_, SimpleType::Zero) => true` — wrong direction (every type was a
  subtype of bottom).
- **gqlite after:** `(SimpleType::Zero, _) => true` matching fppc.
- Verdict: gqlite bug B2 fixed by porting fppc's behavior.

### `LabelType::is_subtype`

- **fppc:** `src/ast/label.rs:43–60`. Has both `(And(l,r), _)` and
  `(Or(l,r), _)` arms, both using `||` (gradual). The `Or-LHS` `||` is a
  fppc-preserved-by-design behavior.
- **gqlite before:** `src/typing/label_type.rs:51–69`. Had `(And(l,r), _)`
  but **no `(Or(l,r), _)` arm**, so subtyping with an `Or` on the left
  fell through to `_ => false`.
- **gqlite after:** added `(LabelType::Or(a, b), _) => is_subtype(a, l2) || is_subtype(b, l2)`
  immediately after the `(And, _)` arm. Uses `||` per fppc.

### `VariableType::is_subtype`

- **fppc:** `src/typechecker/variable_type.rs:264–298`. Has `(Zero, _) => true`,
  `(List, List) => recurse`, `(Union(a,b), _) => || `, `(_, Union(a,b)) => ||`.
- **gqlite before:** missing all four. Fell through to `_ => false`.
- **gqlite after:** added each arm. `(List, List)` is mapped to
  `(Group, Group)` since gqlite uses `Group` for the same role.

### `VariableType::meet` (Union-LHS)

- **fppc:** `src/typechecker/variable_type.rs:227–235`. Combines per-side
  meets via `VariableType::join(v1, v2)` so equal sides dedupe and Zero is
  absorbed (Err handling absorbs failed sides into the surviving one).
- **gqlite before:** built a raw `VariableType::Union(r1, r2)` with manual
  Zero-handling; equal sides did *not* dedupe.
- **gqlite after:** replaced with `VariableType::join(&r1, &r2)`.

### Helpers added in Step 1

- `VariableType::refine_to_nodes(schema, &t) -> Vec<VariableType>` — used by
  `PathType::meet`. Mirrors fppc's `refine_to_nodes`. Returns the
  `Node(_)` variants reachable after schema refinement.
- `Schema` now derives `Debug, Clone` so the typechecker can own a schema
  by value, matching fppc's `Typechecker { schema: Schema, ... }`.

### What stayed gqlite-only (not touched)

- `LabelType::{Top, Empty, Neg}` and the arms that handle them.
- `PropertyType::{Open, Zero}` and the Open/Closed cross-arms.
- `SimpleType::{F, Group, Record}` and their dedicated variants
  (`is_subtype` covariance for `Group` was *added* in Step 1 because it
  mirrors fppc's `List` behavior; the variant itself is gqlite-only).
- `VariableType::{Group, EdgeNonDirectional, EdgeAnyDirection}` semantics.

## Deviations from the Step-0 plan

### 1. Renamed `to_list` → `to_group` in `TypeEnvironment`

fppc has `TypeEnvironment::to_list`. gqlite uses `VariableType::Group` for
repetition grouping (it reserves `List` for user-facing list values), so
the moral equivalent in gqlite is `to_group`. Behavior is identical;
only the name follows gqlite's vocabulary. Recorded in the
`type_environment.rs` doc comment.

### 2. `TypeEnvironment::meet` returns `Result<_, String>` despite gqlite's
`VariableType::meet` being infallible

fppc's `VariableType::meet` returns `Result<_, String>` and `TypeEnvironment::meet`
propagates the error. gqlite's `VariableType::meet` returns `VariableType`
(no `Result`). To preserve the *check-level* observable behavior (so the
checker can report "Concatenation of contexts failed" the way fppc does),
`TypeEnvironment::meet` synthesizes an `Err` when the per-variable meet
collapses to `Zero` for a variable whose inputs were not already empty.
This matches fppc's behavior at the only call site that consumes the
`Err` (the `Concat` branch of `check_path_pattern`).

### 3. `EdgeDir` lives in `path_type.rs`, not in the AST

The source spec's mapping table contemplated a `From<(&VariableType, EdgeDirection)>`
conversion. gqlite has no `EdgeDirection` enum at the AST layer (direction
is encoded in the `PathPattern` variant). I introduced `EdgeDir` as a
typechecker-local enum in `path_type.rs`, and the conversion is named
`PathType::from_variable(t, dir)` rather than implemented as `From<(...)>`.
The dispatch from `PathPattern::EdgeRight/Left/Undirected/AnyDirection` to
`EdgeDir::Right/Left/None/Any` happens in `Typechecker::check_edge`.

### 4. `Join` is treated as `Concat` for typing

Per the source spec, `PathPattern::Join` (gqlite's comma-join) has no fppc
counterpart. The migration doc said "treat as `Concat` for typing,
verify before merge." Verified: comma-join shares variables across
patterns the same way `Concat` does, and the type-level operation in
both cases is "meet the environments under the schema and meet the path
shapes." The runtime distinction (LTJ-decomposable vs. not) is
orthogonal. Both arms share the body in `check_path_pattern`.

If a future test surfaces a case where `Join` and `Concat` should yield
different types, this is the place to revisit.

### 5. `Schema` did not need a `new` constructor

Step-0 plan flagged that fppc's `Schema::new(nodes, edges)` might need a
gqlite analog. gqlite's `Schema` has public fields, so `Schema { nodes,
edges }` literal construction is sufficient. No constructor was added.

## Punts (deliberately incomplete; carry over to phase 2)

| Punt | Where | Notes |
|---|---|---|
| `Expr::FieldAccess` on non-record receivers | `check_expr`, the `_` arm of the Record match | Returns `Zero` and warns. Star/Zero pass through gradually. The user-facing case (a record literal accessed by field) works. |
| `Value::List` and `Value::Record` literal typing | `simple_type_of_value` | Both get loose types (`List(Star)` / `Star`). Recursive value typing wasn't in fppc, and the elaborator hasn't been observed emitting nested literal expressions through to typecheck. |
| Result-row (`RETURN`) typing | `check_query` | The function delegates to `check_pattern`. Aggregates / ORDER BY / LIMIT in RETURN are not typechecked. fppc has no RETURN. |
| Real schema sourcing | `compile_query` | Default is `Schema::star()`; there is no facility yet to load a schema from disk or query metadata. Phase 2 candidate. |
| `NodeId → Type` side-table for the optimizer | n/a | Out of scope per the source spec. |

## Architectural follow-ups still owed (option 1 cost)

- **Parallel typing modules.** `path_type.rs` and `variable_type.rs` carry
  duplicate node info per edge position. fppc has the same shape. The
  refactor (option 2 — demote path-shape to a private struct in
  `checker.rs`) is unblocked once the checker is stable.
- **Per-segment clones at `Concat` / `Union`.** `PathType::meet` /
  `PathType::union` clone subtree boxes. `VariableType::meet` clones
  endpoints and descriptors. Profiled in
  `docs/gqlite_migration_notes.md` as a hot path; phase 1 accepts the
  cost.

Both are *refactorable* on top of the working checker — option 1 sequences
them rather than foreclosing them.

## Findings to flag (no action this phase)

- **`Schema::star()` overload**: gqlite's `Schema::star()` has only
  `EdgeDirectional` + `EdgeNonDirectional` star edges. fppc's
  `Schema::star()` has both directional and undirected star edges (same
  thing under different names). Equivalent. Just noting, in case future
  schema-construction utilities want a richer permissive default.
- **`LabelType::is_empty` always returns `false`** in gqlite. fppc's
  typechecker doesn't use `LabelType::is_empty` at all — the emptiness
  check goes through `DescriptorType::is_empty` → `LabelType::is_empty`,
  but the result is always false. Functionally equivalent to fppc's
  setup. If proper `LabelType` emptiness ever becomes a bottleneck (e.g.
  detecting `A & !A` statically), this is the spot.
- **`gradual_eq` confirmed absent** on both sides; the source spec's
  warning ("must not reappear") was checked at every `SimpleType::meet`
  / `is_subtype` call site in `check_expr`.
- **fppc's preserved-by-design `Or-LHS` `||`** — ported verbatim into
  gqlite's `LabelType::is_subtype`. If fppc ever decides to fix this
  (changing to `&&`), the gqlite-side change is a one-line edit at the
  arm we added in Step 1.

