# IMGpedia / Wikidata: from `.ttl` to a froGQL similarity join

Two dumps go in — a literal-free triple graph and a per-image vector file
— and one `.gdb` plus one vector sidecar come out, against which the
MillenniumDB-dialect similarity queries run directly.

```
graph.ttl  ──import_ttl──►  imgpedia.gdb
hog.ttl    ──vec_build ──►  imgpedia.gdb.vec.hog
                                │
  '?v00 k50 ?v11' ──sparql_to_gql.py──►  GQL  ──►  frogql
```

## 1. The data model

`graph.ttl` holds triples whose every term is a prefixed image name:

```
img:100000001 img:P121 img:33992534 .
```

That maps onto a property graph the obvious way:

| RDF | property graph |
|---|---|
| a distinct image id | one node, label `img`, integer property `id` |
| a triple | one directed edge, label = the predicate's local name (`P121`) |

`hog.ttl` holds one vector per image:

```
img:1164163 <https://www.imfd.cl/hog> "[0,0,0.29078248,...]"^^mdbtype:tensorFloat .
```

Vectors do **not** become properties. They live in a sidecar file
(`<db>.vec.<attr>`) next to the `.gdb`, because a node record has no
extra area and a 288-float property would be decoded by every
`node_props()` call in every query that never looks at it.

## 2. Build the graph

```bash
cargo build --release --bin import_ttl --bin vec_build

./target/release/import_ttl graph.ttl imgpedia.gdb
```

Options: `--node-label img`, `--id-prop id`, `--max-edges N` (sample a
big dump), `--progress N`.

The importer streams the file twice — pass 1 collects the distinct ids
and predicates, pass 2 writes edge records straight onto pages as it
reads. It never builds a `MemoryGraphStore`, which at this scale would
want hundreds of bytes an element.

**Memory.** What has to be resident is the columns the on-disk indexes
are computed from, roughly

```
20 bytes x edges  +  16 bytes x nodes
```

so the full dump (~550 M triples over ~17 M images, extrapolating 22 GiB
at ~40 bytes a line) wants about **12 GiB**. Use `--max-edges` to size a
run first.

Edges carry no element name: every one interns the same empty string, so
the name table holds a single entry rather than half a billion. Nothing
in the query path resolves an edge name.

## 3. Build the vector sidecar

```bash
./target/release/vec_build imgpedia.gdb \
    --attr hog --input-ttl hog.ttl --key img.id --metric l2
```

`--key img.id` resolves each subject through the secondary index froGQL
auto-builds on `(img, id)` — a point lookup per row. `--no-index` skips
the HNSW build, which is worth doing on a first run: the exact sources
(`localsort`, `globalsort`) work without it.

The reader is streaming, so the 47 GiB of text never lands in memory.
What does is the payload the sidecar was always going to hold:

```
4 bytes x dim x rows      (+ ~2 GiB for an HNSW at m=16)
```

At 17.3 M images and dim 288 that is **~20 GiB of vectors**, plus the
index. Nothing about the sidecar format is lazy; this is the floor.

Rows may arrive in any order — they are sorted into the ascending-id
layout the sidecar requires by an in-place cycle permutation, so no
second copy of the payload is allocated.

## 4. Translate a query

```bash
python3 sparql_to_gql.py '?v10 69 ?v00 . ?v10 6 100980834 . ?v01 926 ?v21 . ?v01 69 ?v11 . ?v00 k50 ?v11'
```

```
MATCH (v10:img)-[:P69]->(v00:img),
      (v10)-[:P6]->(:img {id: 100980834}),
      (v01:img)-[:P926]->(v21:img),
      (v01)-[:P69]->(v11:img)
NEAREST 50 v11.hog TO VECTOR(v00, 'hog') AS dist
RETURN DISTINCT v00.id, v01.id, v10.id, v11.id, v21.id, dist
ORDER BY v00.id, dist;
```

The dialect: `?v` is a variable, a bare number is a constant image id, a
bare predicate number `69` means edge label `P69`, and `kN` is the
similarity predicate — `?a kN ?b` reads "`b` is among the `N` images
nearest to `a`".

For a whole benchmark file, one pattern per line:

```bash
python3 sparql_to_gql.py --file q1.tsv --batch --one-line
```

`--batch` treats each line as its own query (without it the file is read
as one pattern, and 100 patterns would merge into one with 100 similarity
predicates). `--one-line` collapses each query onto a single line, which
the REPL needs — it reads a statement per line.

Options: `--attr`, `--label`, `--id-prop`, `--dist`, `--limit`,
`--return`, `--no-distinct`, `--file`, `--batch`, `--one-line`.

`RETURN DISTINCT` is the default because a match is per *physical edge*
under ISO bag semantics, so a pattern with several parallel edges repeats
each logical answer.

### Two shapes the real benchmark file contains

`q1.tsv` (100 patterns) exercises both, and all 100 translate, parse and
typecheck.

**A variable in predicate position** — `?v20 ?v10 142434602` — appears in
16 of them. SPARQL binds that variable to the property IRI; GQL has no
label variable, so it renders as an unlabelled edge:

```
(v20)-[]->(:img {id: 142434602})
```

That is exact **only because** the predicate variable is never used
anywhere else in those patterns, which the translator verifies. A
predicate variable that is also joined on would need label equality, and
quietly dropping it would widen the query; the translator refuses that
case rather than answering a different question.

**A repeated triple** — `?v10 126 ?v00 . ?v10 126 ?v00` — appears in 4 of
them, and is emitted once. A SPARQL basic graph pattern is a *set* of
triple patterns, so the repeat says nothing; a GQL comma-join is not
idempotent under bag semantics, and repeating the operand would multiply
the rows once parallel edges exist.

## 5. Run it

```bash
./target/release/frogql imgpedia.gdb
```

Paste the query on one line (the REPL reads a statement per line).
`FROGQL_DEBUG_VEC=1` prints the arm and its counters:

```
vsearch arm=correlated+hnsw accepted=48 ... pattern_runs=1 anchor_groups=3
```

`pattern_runs=1` with `anchor_groups=3` is the shape to expect: the
pattern is evaluated once and its rows are partitioned by `v00`, one
ranking per partition. `FROGQL_VEC_SOURCE=localsort|globalsort|hnsw`
picks where each partition's ranking comes from; the two exact sources
are the oracle. Full write-up in
`docs/internals/vector-search.md` §*Correlated `NEAREST`*.

## Sample files

`graph_example.ttl` and `hog_example.ttl` are excerpts of the two dumps.
They come from different samples, so their image ids do not overlap — a
`vec_build` run over the pair reports every row as unresolved, which is
correct and not a bug in either tool.
