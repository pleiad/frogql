# froGQL explorer

A `.gdb` opened in a browser tab: schema, queries, results as a table or
as a drawing. No server, no build step, no bundler — two files and the
wasm package.

```bash
wasm-pack build wasm --target web --out-dir explorer/pkg
cd wasm/explorer && python3 -m http.server 8777
```

Then open <http://localhost:8777> and pick a `.gdb`. The `.ltj` sidecar
is optional; see `wasm/README.md` for when it is worth fetching.

## How it is put together

- `worker.js` — the engine. froGQL is synchronous: `open_bytes` on a
  large image is seconds of straight-line work, which on the UI thread is
  a frozen tab, since nothing can paint while wasm holds it. The file
  buffers stay in the worker, so picking the `.ltj` after the `.gdb`
  re-opens without shipping the image across again.
- `index.html` — everything else, in one file. No CDN and no graph
  library: the force layout is ~40 lines, and keeping it here is what
  makes the page work offline and survive being copied somewhere else.

## The layout

Query on the left, schema on the right, results underneath. The schema
started as a third tab beside Tabla and Grafo, which made reading it and
reading an answer mutually exclusive — and reading a schema is what you
do *while* writing a query, so that was asking the reader to remember one
while looking at the other.

Three splitters resize it: sidebar / top row, query / schema, top row /
results. How much schema, query and answer you want on screen at once is
different per database and per question, so the page does not decide it.
Both drawings size their canvas when they are built, so a drag rebuilds
the panes on release rather than stretching a bitmap — cheap, since the
schema is tens of boxes and the result drawing is capped.

## The schema, two ways

**Diagram** — one box per node type, header with the label and a section
listing `prop: TYPE` under it, joined by labelled arrows. Modelled on the
LDBC SNB schema figure, because a schema *is* a graph and a text listing
makes you rebuild its shape in your head. Boxes are sized from their own
contents, so a type with two properties does not get the footprint of one
with ten, and the finished drawing is zoomed to fit its pane (never
magnified past 1:1 — the text is sized for that).

**Text** — byte for byte what `.schema` prints in the REPL
(`SHOW GRAPH TYPE DEFAULT`), coloured with the same palette
`print_schema_colored` uses: labels cyan, value types green, `NULL` and
the open-record `*` magenta, edge brackets yellow. This is the one to
copy into a `CREATE GRAPH TYPE`.

Both come from the active GRAPH TYPE: read from the catalog on a `.gdb`,
inferred from the data on a JSON graph.

### Keeping the lines out of the way

A force layout optimises *distance*. Crossings are a different objective
and no amount of spring tuning turns one into the other, so the forces
settle the spacing and a second pass attacks the drawing directly: swap
two boxes, recount, keep the swap if it helped. Affordable because a
schema is small — node types come in tens, so every pair is a few hundred
trials.

The objective counts two things and weights the second at 3x: **lines
crossing each other**, and **a line drawn straight through an unrelated
box**. A crossing is a knot the eye untangles; a line over a box hides
text. Measured on the LDBC schema (11 types, 25 relationships): **34
crossings down to 3**. On a 5-type OpenStreetMap schema: 0 crossings, 0
lines over boxes, 0 overlapping boxes.

Edge captions then take the first point along their line that is clear of
every box rather than the midpoint come what may, and a type with several
relationships to itself gets one ring per relationship with the labels
stacked around its centre.

## The typechecker answers before the runtime does

froGQL's point is that a pattern is checked against the graph's type
before it runs, and a bare "0 filas" throws that away: it reads as *no
such data* when the truth may be *this pattern cannot match*. A strip
under the editor says which, updated while you type.

`Connection::check(query)` runs parse → elaborate → typecheck with no
optimizer pass and no graph access, so it is cheap on every keystroke,
and returns what the pipeline otherwise drops:

- **`empty`** — the schema proves the pattern matches nothing. This is
  the case that sends a reader hunting for missing data: on a schema
  where every `EN_CALLE` points *into* `Calle`, `(n:Calle)-[e]->(m)` is
  provably empty, and the diagram beside it shows why.
- **errors**, separated into syntax and type.
- **warnings**, such as a label that is not in the schema.
- **`vars`** — what the checker *inferred* for each variable, which is
  the part no amount of re-reading the query tells you. An impossible
  variable shows `⊥`.

Inferred types are shown short: an unconstrained edge variable types as
the union of every edge type in the schema, properties included, which
buries the one thing worth reading. Labels are kept, property blocks
dropped, unions past two summarised — the full text is on the tooltip.

The pipeline is run directly rather than through
`compile_query_with_diagnostics_with`, which returns the compiled query
and discards the `TypeEnvironment`.

## A colour means one thing everywhere

Each node type gets a hue, assigned once from the schema. The schema
box's header, the dots in a result graph and the label chips in the
sidebar all read that one assignment, so a blue dot and the blue box are
the same type without anything having to say so. Two assignments that
agree today would not stay agreeing, which is why there is one.

An element whose labels match no declared type keeps the neutral accent
rather than borrowing a colour that would claim a type it does not have.

## Clicking a node shows everything in it

The caption on a drawn node is its labels plus two properties — enough to
tell nodes apart, not enough to inspect one. A click opens a card with
every label and every property, sorted. The whole element is kept when
the drawing is built, so the click costs no round trip.

A click and a drag are the same gesture until the pointer travels more
than a few pixels — **not** until it moves at all. Pressing a mouse
button nudges the pointer, so "any movement is a drag" meant no real
click ever opened the card. Synthetic events do not jitter, which is
exactly why that survived being tested.

The card is sized by the window rather than by the pane it floats over:
once the splitters are dragged the results pane can be a couple of
hundred pixels tall, and a card capped at that showed three properties of
eighteen.

## Resizing does not rearrange anything

A force layout is seeded randomly, so re-running it gives a different —
equally valid, entirely unfamiliar — arrangement. Re-running it on every
splitter drag scrambled a diagram the reader had just finished reading.
Positions are kept; a resize only re-sizes the canvas and re-fits the
zoom. The `↻` button asks for a fresh arrangement deliberately, and a new
database or a new answer gets one on its own.

## The editor is highlighted

Keywords, `:Labels`, strings, numbers and `--` comments, painted by a
`<pre>` under a transparent `<textarea>` that shares its metrics. No
editor library: the grammar worth colouring is six token classes, and a
dependency would cost more than it saves in a page meant to be copied
around.

## The result drawing is a view of an answer, not of the graph

Nodes and edges come from `_paths`, which a query returns when it has no
`RETURN` clause — the elements the pattern walked, in match order. A
query that projects columns has no `_paths`, and the graph tab says so
rather than drawing nothing.

Edges carry arrowheads, and the direction comes from the edge rather than
from the order the path walked it — `(a)<-[e]-(b)` traverses b→a while
the arrow still belongs on a→b, so following the traversal would point
half of them backwards. An undirected edge gets no head, and a self-edge
becomes a small loop. This needs `src`, `tgt` and `directed` on the edge
element, which the binding did not send: a path is a sequence of
elements, and `EdgeDirectional` / `EdgeUndirectional` both arrive as
`kind: "edge"`.

At most **120 nodes** are drawn. The first version allowed 300 and the
result was a texture rather than a picture; the cap is low on purpose and
the notice says how to get under it. Labels fade in with zoom: unreadable
at a distance, wanted once a node is close enough to be the thing you are
looking at.

Element ids are never shown. They are not stable across a `save`, so they
are not what a reader wants; an element renders as its labels plus its
first couple of properties.

## What it does not do

- **No pagination.** Queries run with a 500-row cap.
- **No IndexedDB.** Pick the file each time.
- **A 4 GB ceiling.** `wasm32` addresses 4 GB and a tab gets less. A
  309 MB database is fine; a 617 M-edge one is not close.
