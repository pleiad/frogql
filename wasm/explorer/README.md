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

## The drawing is a view of an answer, not of the graph

Nodes and edges come from `_paths`, which a query returns when it has no
`RETURN` clause — the elements the pattern walked, in match order. A
query that projects columns has no `_paths`, and the graph tab says so
rather than drawing nothing.

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
