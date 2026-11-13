# SPARQL frontend (design)

Status: **phase 1 (MVP) implemented** in `src/sparql/`, behind the
`sparql` feature (on by default). Phases 2 to 5 are open. Decisions taken
so far are marked **Decided**; the rest are open.

## Using it

- Rust: `frogql::compile_sparql_with(&schema, text, &mapping)` returns the
  same `CompileResult` a GQL query compiles to;
  `Connection::query_sparql(text, &mapping, limit)` runs one.
- CLI: `sparql_query <db.gdb> <query.rq | -> [options]` prints the rows;
  `--explain` prints the GQL the query translates to (constant-IRI key
  filters are not shown: `Descriptor`'s `Display` omits value filters),
  `--gql` runs a GQL file instead, for an A/B on the same database.
- Not yet in the REPL or the Python / Node / WASM bindings (phase 5).

**The mapping** (`SparqlMapping`) says how RDF terms were laid out in the
graph, because the graph does not say:

| field | default | `SparqlMapping::import_ttl("img", "id")` |
|---|---|---|
| `predicates` | `FullIri`: edge label = whole IRI | `LocalName`: `…/P121` → `P121` |
| `node_key` | `Iri { prop: "iri" }`: string property, whole IRI | `LocalNameInt { prop: "id" }`: integer local name |
| `node_label` | none | `img` |

A SPARQL variable denotes an RDF term, so in an expression or a projection
it reads the node's key (`x.iri`, `x.id`), not the node. The key is unique,
so equality on it is node identity. A constant IRI becomes a hidden node
variable with a key filter (`(_hc1:img {id: 5})`), one per distinct IRI, so
two mentions of a resource are one node to the join. With a `node_label`,
that filter is a point lookup in the auto-built `(label, key)` index;
without one the engine has no index to ask and scans.

## Phase 1 coverage

| | |
|---|---|
| supported | `SELECT` (`*`, variables, `(expr AS ?v)`), `DISTINCT`, `REDUCED`, `LIMIT`, `ORDER BY` (unbound sorts first, as SPARQL ranks it) |
| | BGP, `Join`, `OPTIONAL` (with its own `FILTER`), `UNION` of branches binding the same variables, `FILTER` |
| | property paths `^ / \| !` and `* + ?`, the closure in a sequence split at a hidden node so only the closure is per pair |
| | `GROUP BY` variables, `COUNT` / `SUM` / `AVG` / `MIN` / `MAX` (with `DISTINCT`), `COUNT(*)`, `SAMPLE` (as `MIN`) |
| | expressions: comparisons, `&& \|\| !`, arithmetic, `IN`, `BOUND`, `IF`, `COALESCE`, `EXISTS`, `NOT EXISTS`; literals of the XSD integer, decimal, double, float, boolean and string types |
| | a predicate variable mentioned once (`?s ?p ?o`), as a wildcard edge |
| error, by name | `OFFSET`, `ASK`, `CONSTRUCT`, `DESCRIBE`, `FROM`, `GRAPH`, `SERVICE`, `MINUS`, `VALUES`, `BIND` inside a group, subqueries, `HAVING`, `GROUP BY` an expression, `COUNT(DISTINCT *)`, `GROUP_CONCAT` |
| | literals in triple patterns, language-tagged literals, other datatypes, SPARQL functions (`STR`, `REGEX`, …) |
| | a predicate variable mentioned twice (phase 2), `UNION` branches binding different variables, an `OPTIONAL` inside `OPTIONAL` or `UNION` |
| | a `FILTER` over a variable only an `OPTIONAL` binds (a clause's `WHERE` belongs to that clause, so no clause of the chain expresses a filter on the left join's result) |
| | a `*`, `+` or `?` inside an alternative or another closure |

Tests: `tests/it/sparql_test.rs` (each query against the answer SPARQL
defines, and where GQL can say the same, against that GQL on the same
graph) and the unit tests in `src/sparql/mod.rs`.

**Measured** (release build, LDBC SF0.1, 327 K nodes / 1.48 M edges,
mapping `--local-names --node-label Person --key id --int-key`): four
queries (a two-triple BGP on a constant person, a `likes/hasCreator`
sequence with `DISTINCT`, a `GROUP BY` + `COUNT` + `ORDER BY` + `LIMIT`,
and `replyOf+`) return the same rows as their hand-written GQL twins, at
the same latency (1.1 to 2.4 ms; 2.2 s for the `replyOf+` one, in both).
The 66-triple IMGpedia sample `wikidata/graph_example.ttl`, imported with
`import_ttl` and queried under `--import-ttl`, gives the counts `awk`
gives on the file.

## Goal

Accept SPARQL 1.1 `SELECT` queries and run them on the froGQL engine,
without a second engine. A SPARQL query is compiled to the same AST a GQL
query produces (`syntax::query::Query`, `syntax::path_pattern::PathPattern`)
and then goes through the existing `elaborate → typecheck → optimize → run`
pipeline.

The fit is good because the LTJ was built for this shape: `TripleIndex`
holds every edge as a `(s, p, o)` triple in six orderings (the CompactLTJ
paper is an RDF/BGP paper). A SPARQL basic graph pattern is a set of triple
patterns, which is what `try_ltj` already joins.

## Decisions

- **Decided: compile to the AST, not to GQL text.** An IRI such as
  `http://x/y#z` is not a valid label token in GQL text, so a text
  translation would need an escaping layer the AST does not. It also skips
  a parse. For tests and debugging, a printer from the AST back to GQL text
  is enough (and lets a test compare a SPARQL query with its hand-written
  GQL twin).
- **Decided: use an existing parser.** `spargebra` (the SPARQL parser of
  Oxigraph, MIT/Apache-2.0) parses SPARQL 1.1 into the SPARQL algebra
  (`GraphPattern::{Bgp, Path, Join, LeftJoin, Filter, Union, Extend, Minus,
  Values, OrderBy, Project, Distinct, Reduced, Slice, Group, ...}`) with
  prefixes and IRIs already resolved. The frontend is then a translation
  from that algebra to our AST, not a parser.
- **Decided: a node is found by a key property, named by the mapping.** A
  constant IRI in a triple pattern (`<http://x/123> :p ?o`) must resolve to
  a node. The IRI only survives as the node's external name, and no query
  reads the name table (see *GraphAccess trait* in `CLAUDE.md`). The
  default key is an `iri` string property; the auto-builder indexes it (it
  is unique within the label), so a constant IRI is a hash point lookup.

  **Cost found while implementing:** every distinct string property value
  is resident in the shared dictionary, so a whole-IRI key holds every IRI
  of the graph in RAM: an estimate, not a measurement, is ~7 GB of string
data alone for IMGpedia's 160 M nodes at ~45 bytes per IRI. The
  `import_ttl` layout (an integer local name under `id`) does not pay it,
  which is why `import_ttl` was left as it is and the mapping can name
  either key. A literal-aware importer (phase 3) has to choose.
- **Decided: string functions are shared with GQL.** `FILTER` needs string
  functions, and GQL lacks them too (`docs/frogql-for-agents.md` §12). They
  are implemented once as `Expr::Call` builtins in `engine.rs::eval_call`;
  SPARQL names map onto them.

## Dependency notes

**Decided:** `spargebra` 0.4.8 sits behind an optional `sparql` feature
and the crate keeps `rust-version = 1.71`. That number is the floor of the
library *without features*; `repl` and `bench` already need a newer
stable, and `sparql` joins them (`spargebra` 0.4.8 declares 1.87). The
alternative, pinning `0.3.0-alpha.5` (what `cargo info` resolves under
1.71), would have meant building on an alpha.

The Python, Node and WASM builds (all `default-features = false`) do not
see it until they opt in. Its dependencies
are `oxrdf`, `oxiri`, `oxilangtag`, `peg`, `rand`, `thiserror`; `rand`
needs `getrandom`, which needs care on `wasm32-unknown-unknown` if the WASM
binding ever enables the feature.

## Translation

| SPARQL algebra | froGQL AST |
|---|---|
| triple pattern `?s :p ?o` | `Concat(Node(s), EdgeRight(:p), Node(o))` |
| `Bgp` (several triples) | `Join` of the triples, which `try_ltj` runs as one LTJ |
| `Join` | `Join` |
| `Filter` | `Filter` / `WHERE` |
| `LeftJoin { expression }` | `OPTIONAL MATCH ... WHERE expression` |
| `Union` | path union `\|` (see open questions) |
| `Project` / `Distinct` / `Slice` / `OrderBy` | `RETURN` / `DISTINCT` / `LIMIT` / `ORDER BY` |
| `Group` + aggregates | `GROUP BY` + `Expr::Agg` |
| path `^p` | `EdgeLeft(:p)` |
| path `p1/p2` | `Concat` |
| path `p1\|p2` | edge label `:p1\|p2` |
| path `!p` | edge label `:!p` (`LabelType::Neg` exists) |
| path `p+` / `p*` / `p?` | `ANY SHORTEST (s)-[:p]->+(o)` / `{0,}` / `{0,1}` |
| constant IRI in subject/object | node with `WHERE x.iri = '<iri>'` |
| constant IRI in predicate | edge label |

**`p+` and `p*`.** SPARQL evaluates an arbitrary-length path with set
semantics: one solution per connected `(s, o)` pair, however many paths
connect them. `ANY SHORTEST` partitions by `(first node, last node)` and
keeps one path per partition, which is the same thing, and it is a prefix
the typechecker accepts for unbounded repetition. The BFS fast path
(`try_shortest_bfs`) covers the single-edge case.

The prefix must cover the closure and nothing else. Around `:a/:b+` it
would also collapse the multiplicity of the plain `:a` step, which SPARQL
keeps (a sequence is a join at a hidden node). So a sequence holding a
closure is split into comma operands at a hidden middle node, and only the
closure's operand carries `ANY SHORTEST`
(`a_sequence_keeps_the_multiplicity_of_its_plain_steps`).

## The predicate variable (`?s ?p ?o`)

A single triple with a variable predicate is `(s)-[p]->(o)`, and that runs
today. Two things break as soon as `?p` is used as SPARQL uses it:

1. **Projection.** In GQL `p` is the *edge*, an element with identity. In
   SPARQL `?p` is the predicate IRI, a value. `SELECT ?p` must return
   `:knows`, while `RETURN p` returns the edge.
2. **Joins on `?p`.** `?a ?p ?b . ?c ?p ?d` means "the same predicate".
   `(a)-[p]->(b), (c)-[p]->(d)` means "the same edge", which forces `a = c`
   and `b = d`. Wrong rows, no error.

Two ways to fix it:

**A. In the translator only.** Give each triple its own edge variable and
compare labels afterwards:
`(a)-[e1]->(b), (c)-[e2]->(d) WHERE LABEL(e1) = LABEL(e2)`.
Needs only a `LABEL(e)` function. Correct, but the comparison runs after
the join: the LTJ first produces every pair of edges the rest of the
pattern allows, and the filter throws most of them away. With both
triples otherwise free that is `|E|²` rows.

**B. A label variable in the engine (preferred).**
`pattern_extract::build_p_term` already turns an edge with no fixed label
into an LTJ variable on the predicate level, but always a **fresh** one per
triple (`fresh_var`). The fix is to let an edge carry a *label variable*
(a field on `Descriptor`, e.g. `label_var: Option<String>`) and have
`build_p_term` look it up in `names` instead of minting a fresh one. Two
triples naming the same label variable then share one LTJ variable, and
the leapfrog intersects on the predicate level like on any node: for
`<x> ?p ?o1 . <y> ?p ?o2` it walks only the predicates `x` and `y` have in
common.

What B needs besides that:

- **Binding.** The LTJ binds a label id (`u32`). `convert_results` turns
  it into `Value::Str(label name)` in `Assignment.scalars`, the slot for
  clause-bound scalars, so `RETURN p` / `SELECT ?p` yield the IRI.
- **Typing.** The label variable types as `VariableType::Scalar(Str)`,
  the terminal variant `NEAREST ... AS d` already uses.
- **Fallback.** A pattern the LTJ cannot decompose (hash-join path) must
  still honour the shared variable, by lowering it to A's filter. Slow but
  correct, and only on shapes that already leave the LTJ.
- **GQL surface.** ISO GQL has no label variables, so this is a froGQL
  extension with syntax still to choose. Worth having: "which relation
  links these two nodes" is a common question in GQL too.

Out of scope here: `?p` in subject or object position (`?p rdfs:label ?l`),
which treats the predicate as a node and needs a label-to-node mapping.

## Literal representation (open, to be evaluated)

`import_ttl` drops every triple whose object is a literal. Three ways to
store `:s :name "a" . :s :name "b"`, to be implemented as importer modes
and compared on the same data.

### A. Scalar property: `s.name = "a"`

- Pro: smallest; a literal filter is a `value_pred` and can use a
  secondary index (hash or btree).
- Con: one value per key. The second `:name` is lost, or must win by an
  arbitrary rule.
- Con: datatype and language tag are lost unless encoded in the value.
- Con: `?s ?p "a"` (variable predicate, literal object) is not a triple
  any more, so the LTJ cannot answer it.

### B. List property: `s.name = ["a", "b"]`

- Pro: keeps every value with one record read per node.
- Con: lists are not indexable (`IndexKey` covers `Int`, `Float`, `Str`,
  `Bool`), so a literal filter is a scan.
- Con: `?s :name ?n` must produce one row per element. froGQL has no
  `UNWIND`, so this needs a new unnesting operator.
- Con: the key's type is either always a list (even for single values) or
  `T | LIST<T>`, which every reader then has to handle.
- Con: same as A for datatype, language tag and `?s ?p "a"`.

### C. Literal as node: `(s)-[:name]->(:Literal {value: "a", datatype, lang})`

- Pro: uniform. Every SPARQL triple is an edge, so the LTJ answers every
  BGP, `?s ?p "a"` included.
- Pro: multiple values, datatype and language tag are kept.
- Pro: with one node per distinct literal, a join on a literal
  (`?a :age ?x . ?b :age ?x`) is a join on a node id, inside the LTJ.
- Con: largest (one node per distinct literal, one edge per literal
  triple).
- Con: a `FILTER` on the value reads a property of another node. Indexable
  only if `(Literal, value)` passes the auto-builder's uniqueness rule,
  which fails when one value appears with two datatypes; a declared
  `CREATE INDEX` avoids that.

### Evaluation plan

- Importer flag `--literals scalar|list|node` in `import_ttl` (and drop
  the literal-free restriction).
- One RDF dataset with literals, multi-valued predicates and language tags
  (IMGpedia has none; a Wikidata or DBpedia subset does).
- One fixed query set covering: BGP with a literal filter, a multi-valued
  predicate, a join on a literal, a variable predicate with a literal
  object, a language-tag filter.
- Measures: import time, `.gdb` size, open time, latency per query, and
  correctness against a reference engine. Oxigraph (same family as
  `spargebra`, in-memory store) could be a dev-only oracle; the
  MillenniumDB runner in `bench/cross-system/` is another.

Expectation, to be checked: C is the only one that answers every query
correctly; A wins on size and filter latency where it is correct.

## Semantic differences to watch

- **Sets vs bags.** An RDF graph is a set of triples; a property graph can
  hold the same `(s, p, o)` twice. If the importer keeps duplicates, every
  BGP over them over-counts. Deduplicate at import.
- **`UNION` with different variables.** SPARQL allows each branch to bind
  different variables (the others stay unbound). Phase 1 maps `Union` onto
  `|` only when both branches bind the same variables and refuses the
  rest; what froGQL's path union does with a variable present in one arm
  only is still to be checked.
- **Unbound vs null.** An unbound SPARQL variable maps to `NULL`. `BOUND(?x)`
  maps to `x IS NOT NULL`.
- **Effective boolean value and errors.** A SPARQL `FILTER` that raises an
  error drops the row; froGQL empties the path on a type error, which agrees
  for `WHERE`.
- **Blank nodes** become nodes with a generated `iri` (`_:b0`).

## String functions (shared with GQL)

SPARQL needs at least `STR`, `STRLEN`, `UCASE`, `LCASE`, `CONTAINS`,
`STRSTARTS`, `STRENDS`, `REGEX`, `LANG`, `CONCAT`. The GQL surface should
use the ISO GQL names for character string functions; **verify those names
against the standard before implementing** (none are documented in
`iso-gql-gaps.md` yet). Functions with no ISO counterpart (`REGEX`,
`CONTAINS`) are a froGQL extension and get documented as such, like
`NODES` / `EDGES`. Every one also goes in `docs/frogql-for-agents.md`.

## Out of scope for the first version

`CONSTRUCT`, `ASK`, `DESCRIBE`, named graphs (`GRAPH`), `SERVICE`,
`MINUS`, `VALUES`, `BIND`, subqueries, SPARQL Update.

## Phases

1. **MVP** (no literals), **done**: see *Phase 1 coverage*. Two changes
   from the plan: no `iri` property in `import_ttl` (its RAM cost, above;
   the mapping reads `import_ttl`'s integer key instead), and the entry
   point takes the mapping (`compile_sparql_with(&Schema, &str,
   &SparqlMapping)`). The full IMGpedia graph lives on another machine, so
   the real-data check ran on LDBC SF0.1 and the IMGpedia sample.
2. **Predicate variable**: label variable in the AST and the LTJ,
   `LABEL(e)`, GQL surface for both.
3. **Literals**: the three importer modes and the evaluation above.
4. **String functions**, shared with GQL.
5. **Surface**: REPL (detect a leading `PREFIX` / `SELECT`, or a `.lang`
   switch), `Connection::execute_sparql` in Rust, Python and Node, and
   SPARQL JSON results output.
