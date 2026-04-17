# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

GQLite — a Rust graph database implementing ISO GQL path pattern matching with single-file storage. Part of a research project; the Python reference implementation is in `../pygql/`.

## Commands

```bash
# Tests (129 passing, skip bench_test which has pre-existing failures)
cargo test --test parser_test --test runtime_test --test store_runtime_test --test text2gql_test

# Single test
cargo test --test runtime_test test_join_star_any_label -- --exact

# Build all binaries
cargo build --release

# Interactive REPL
./target/release/gqlite movies.gdb --import-csv path/to/csv_dir/   # create + open
./target/release/gqlite movies.gdb                                  # open existing

# Python bindings (builds cdylib, installs `gqlite` into the active venv)
cd python && source ../../pygql/.venv/bin/activate && maturin develop --release
```

## Workspace layout

`gqlrust/` is a Cargo workspace with two members:
- `.` (root) — the `gqlrust` library crate + CLI binaries (`gqlite`, `bench_queries`, `convert_edgelist`)
- `python/` — the `gqlite-py` crate: a `cdylib` exposing a PyO3 extension module named `gqlite`. Depends on `gqlrust` via path. Built and installed with maturin (`maturin develop` for local dev, `maturin build --release` for wheels). Maturin installs into whichever venv is active; defaults to `.venv` in the package dir if present.

Python API surface (`python/src/lib.rs`): `gqlite.open(path)`, `gqlite.import_json(db, json)`, `gqlite.import_csv(db, dir)`, and a `Connection` class with `execute(query, limit)`, `schema()`, `node_count`, `edge_count`. `execute` returns a list of dicts: RETURN clauses produce `{alias: value}` rows; queries without RETURN produce raw `{var: {kind, id, labels, props}}` dicts. `Connection` is `unsendable` (not thread-safe across Python threads).

## Architecture

### Compiler pipeline

`parse → elaborate → optimize → run`. Elaboration (`src/elaborate/`) performs
ISO-mandated semantic lowering: `(x:L {k: v})` becomes `(x:L) WHERE x.k = v`.
The `:` vs `is` split inside descriptors distinguishes value filters from type
ascriptions — `{name is str}` stays in the descriptor's `PropertyType`, while
`{name: 'Alice'}` is hoisted. Descriptors carry a `value_filters` field that
the parser populates and elaboration drains; after elaboration it is always
empty. The optimizer is reserved for performance-preserving transforms
(predicate pushdown, label-index selection, LTJ join rewriting).

### Entry points

- `compile(query) → PathPattern` — parse + elaborate + optimize a path pattern string
- `compile_query(input) → Query` — parse a full `MATCH ... WHERE ... RETURN` query
- `Runtime::new(graph).run(&pattern)` — execute against any `GraphAccess` backend
- `Runtime::run_query(&query, limit)` — execute with RETURN projection
- `Runtime::run_with_limit(&pattern, limit)` — early termination after N results

### ID system

All node/edge IDs are `u32` internally (`pub type Id = u32` in `model/value.rs`). String names exist only for display via `node_name(id)` / `edge_name(id)` on the trait. `PathValue` variants are `Node(u32)`, `EdgeDirectional(u32)`, `EdgeUndirectional(u32)`, `Nothing`, and `List(Vec<PathValue>)` (for repetition grouping only). PathValue is NOT Copy because of the List variant.

### GraphAccess trait

The runtime is generic over `GraphAccess`. Node and edge methods are separate: `node_labels(id)` / `edge_labels(id)`, `node_props(id)` / `edge_props(id)`. The runtime knows which to call from context (filtering nodes vs edges). Three backends:
- `Graph` — in-memory from JSON, all data in RAM
- `LazyGraphStore` — topology (edge_src/tgt) + label index in RAM, labels/props read from disk via LRU page cache. No string names in memory.
- `DiskGraphStore` — topology in RAM, everything else from disk

### Parser grammar hierarchy

```
full_query  = MATCH? query (WHERE expr)? (RETURN items)?
query       = path_pattern ("," path_pattern)*     ← Join (lowest precedence)
path_pattern = path_term ("|" path_term)*           ← Union
path_term   = path_factor+                          ← Concat (juxtaposition)
path_factor = path_primary quantifier?              ← Repeat {n,m}
```

`MATCH` keyword is optional — bare path patterns like `(x)-[]->(y)` still work. The `AS` keyword is ambiguous between type cast (in expressions) and alias (in RETURN); `return_comparison()` excludes `AS` from operators so it's available for aliases.

### Join strategy: Leapfrog Triejoin (LTJ)

El runtime usa Leapfrog Triejoin (LTJ) como estrategia principal para evaluar joins y concatenaciones de aristas dirigidas. LTJ es un algoritmo worst-case optimal para multi-way joins: vincula variables una a la vez intersectando listas de candidatos de todos los patrones simultáneamente, sin materializar resultados intermedios. La implementación sigue el paper CompactLTJ (Arroyuelo et al., VLDBJ 2025), con la referencia C++ en `../cltj/`.

**El problema que resuelve.** La estrategia anterior (hash-join pairwise) materializaba ambos lados de cada join antes de unirlos. Para multi-way joins como un 4-clique con 6 sub-queries, los resultados intermedios crecen exponencialmente: un nodo con grado 100 produce 10K pares en el primer join, 1M triples en el segundo, etc. La mayoría se descarta en joins posteriores. LTJ elimina este blowup.

**Idea central: todo es un triple.** Cada arista dirigida del grafo se modela como un triple `(src, label, tgt)`, igual que en RDF. Estos triples se almacenan en 6 ordenamientos sorted (`TripleIndex`): SPO, SOP, POS, PSO, OSP, OPS. Cada ordenamiento permite buscar eficientemente por cualquier prefijo de sus componentes usando binary search.

#### Transformación de queries a triples

La clave de la implementación es que tanto los comma-joins como las concatenaciones de aristas se descomponen en conjuntos de triples. Un concat es simplemente un join sobre nodos intermedios compartidos.

**Ejemplo 1: cadena de aristas.** La query `(a)->(b)->(c)->(d)` se descompone en:

```
Triple 1: (a, _p0, b)    ← primera arista
Triple 2: (b, _p1, c)    ← segunda arista
Triple 3: (c, _p2, d)    ← tercera arista
```

Las variables `_p0`, `_p1`, `_p2` son variables frescas para labels (matchean cualquier label). La variable `b` es compartida entre triple 1 y 2; `c` entre 2 y 3. LTJ vincula las variables en un orden inteligente (VEO) e intersecta candidatos en cada paso.

**Ejemplo 2: triangle (3-clique).** La query `(a)->(b), (b)->(c), (c)->(a)` se descompone en:

```
Triple 1: (a, _p0, b)
Triple 2: (b, _p1, c)
Triple 3: (c, _p2, a)
```

Aquí `a` aparece en triples 1 y 3, `b` en 1 y 2, `c` en 2 y 3. LTJ hace:

```
para cada a:
  para cada b en out(a):         ← intersecta triples 1 y 2
    para cada c en out(b) ∩ in(a):  ← intersecta triples 2 y 3
      emit (a, b, c)
```

Nunca materializa "todos los pares (a,b)" antes de considerar `c`.

**Ejemplo 3: labels concretos.** Si la arista tiene label, como `(x)-[:Transfer]->(y)`, el label se codifica como constante en el triple: `(x, Transfer_id, y)`. El iterator pre-fija esta constante y solo explora aristas con ese label.

**Ejemplo 4: nodos anónimos.** Los nodos sin variable como `()` reciben variables frescas internas (`_ltj_0`, `_ltj_1`, ...) que participan en el join pero se excluyen del resultado final.

#### Cómo funciona el algoritmo

1. **Flatten**: el árbol de `Concat` left-asociativo se aplana en una lista `[Node, Edge, Node, Edge, Node, ...]`.
2. **Extracción de triples**: cada par `(Node, Edge, Node)` consecutivo produce un triple.
3. **TripleIndex**: se construye un índice con 6 ordenamientos sorted de todos los triples del grafo. Cada entrada es `(u32, u32, u32, u32)` = (comp0, comp1, comp2, edge_id).
4. **Iteradores**: cada triple del query tiene un `LtjIterator` que navega el ordering apropiado. Las constantes del triple se pre-fijan, y el iterator selecciona el trie correcto según qué posiciones (S/P/O) ya están fijadas.
5. **VEO (Variable Elimination Order)**: determina el orden en que se vinculan las variables. Las variables compartidas entre múltiples triples (non-lonely) se vinculan primero; las que aparecen en un solo triple (lonely) al final.
6. **Leapfrog seek**: para vincular una variable, se rota entre todos los iteradores que contienen esa variable, llamando `leap(c)` (binary search para el menor valor ≥ c). Cuando todos coinciden en el mismo valor, ese valor es un candidato válido.
7. **Descenso recursivo**: al vincular una variable, se hace `down()` en todos los iteradores, se recurre para la siguiente variable, y luego `up()` para backtrackear.

#### Filtros integrados en el loop

Los filtros (labels de nodos como `(x: Person)`, constraints de properties) se colocan en el nivel del VEO donde todas sus variables ya están vinculadas. Se evalúan antes de descender al siguiente nivel, podando subárboles enteros. Por ejemplo, si `x: Person` falla para `x=5`, no se explora ningún valor de `y` ni `z`.

#### Cuándo se activa LTJ y cuándo no

LTJ se activa automáticamente en `run_join` y `run_concat_pattern` si el pattern es descomponible en triples:

- **Sí**: cadenas de aristas dirigidas, comma-joins de aristas dirigidas, combinaciones de ambos, aristas con o sin label
- **No**: aristas no dirigidas (`~[e]~`), aristas en dirección izquierda (`<-[e]-`), uniones (`|`), repeticiones (`{n,m}`), filtros WHERE complejos

Si la descomposición falla, el runtime usa el fallback (hash-join pairwise para joins, adjacency-driven concat para concatenaciones). Esto garantiza cero regresión.

#### Limitaciones actuales

1. **Repeticiones**: `{n,m}` no se descompone en triples (se podría unrollear para bounds fijos, pero no está implementado). Se usa el runtime existente.
2. **Aristas no dirigidas/izquierda**: no se modelan como triples actualmente.
3. **WHERE expressions**: los filtros de labels de nodos están integrados; WHERE con expresiones complejas (`x.age > y.age`) se aplica como post-procesamiento.
4. **TripleIndex se reconstruye por query**: no hay caché entre queries (se podría cachear en Runtime con `RefCell`).
5. **VEO es estático**: el orden de variables se fija antes de la búsqueda. Un VEO adaptivo (que re-estima cardinalidades durante la búsqueda) mejoraría queries donde la selectividad varía.

#### Resultados en benchmarks (soc-LiveJournal1-100k, limit=1000)

| Query | Hash-join (antes) | LTJ (ahora) | Speedup |
|-------|-------------------|-------------|---------|
| 3-clique | 0.57s | 0.041s | 14x |
| 4-cycle | 4.15s | 0.041s | 101x |
| 3-path | 6.33s | 0.040s | 158x |
| 2-tree | 101.8s | 0.039s | 2610x |
| 4-path | 159.8s | 0.039s | 4097x |
| 4-clique | colgaba | 0.043s | ∞ |

#### Estructura del módulo `runtime/ltj/`

```
ltj/
  mod.rs              — declaraciones
  triple_index.rs     — TripleIndex: 6 ordenamientos sorted, binary search, leap, range queries
  iterator.rs         — LtjIterator: navegación con constants pre-fijadas, leap/down/up
  veo.rs              — VeoSimple: orden fijo por peso, non-lonely primero
  algorithm.rs        — LtjAlgorithm: leapfrog seek/search, LtjRunner con filtros
  pattern_extract.rs  — flatten de concats, descomposición en triples, integración con engine
```

### Comma-join fallback (hash-join pairwise)

Cuando LTJ no puede descomponer un join (por contener uniones, repeticiones, o aristas no dirigidas), se usa el hash-join pairwise original. Evalúa ambos lados completamente, construye un hash index sobre la primera variable compartida, y produce el cross-product filtrado. Multi-way joins como `Q1, Q2, Q3` son left-associative: `Join(Join(Q1,Q2), Q3)`.

### Repetition and PathValue::List

`-[x]->{n,m}` binds `x` to a `List` of matched edges, not a single edge. `to_group()` wraps each value in a singleton list, `concat_group()` concatenates lists. Nested repetitions produce nested lists: `(-[x]->{1,2}){1,2}` gives `x ↦ [[e1], [e2, e3]]`. The zero-repetition base case fills variables with empty lists.

### CSV loader

`csv_loader::load_from_csv_dir(path)` reads `spanner_import_config.json` to discover node/edge files. Node files are identified by NOT having SRC_ID/DST_ID columns (case-insensitive). The ID column is found by trying: `vid`, `<Label>_id` (case-insensitive), any `*_id` column, then first column. Edge labels are inferred by stripping known node type names from the config's `label` field or the filename. All column lookups are case-insensitive.

### Storage format (.gdb files)

4KB pages, slotted-page layout for variable-length records. Header page 0 stores root pointers to string table, label indexes, and adjacency index. Node/edge records reference strings by string table ID. Adjacency index maps node_id → Vec<(edge_id, other_node, kind)> where kind is 0=outgoing, 1=incoming, 2=undirected. See `docs/storage-architecture.md` for full specification.

### Optimizer

- **Leapfrog Triejoin**: multi-way join + concat optimization (see above)
- **Predicate pushdown**: extracts `x.attr is type` from WHERE conjunctions and merges into descriptors
- **Label index selection**: picks smallest indexed set for compound labels like `A&B` via `LabelType::required_labels()`

## Key conventions

- Labels in patterns require `:` prefix: `-[:Transfer]->` not `-[Transfer]->`
- The `bench_test` has pre-existing failures — exclude it from regular test runs
- The `bench/data/` directory is gitignored (large datasets, download via scripts)
- Example databases in `examples/*.gdb` ARE committed (small, useful for testing)
- The parent `../CLAUDE.md` covers the full monorepo (pygql, playground, gqlrust)
