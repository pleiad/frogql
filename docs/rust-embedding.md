# Embedding froGQL in Rust

froGQL is an embedded database: your program opens a `.gdb` file and runs
GQL against it in-process, with no server, like SQLite. This guide covers
the Rust API. Python and Node have the same surface (see
[`frogql-for-agents.md`](frogql-for-agents.md) §2).

## Add the dependency

```toml
[dependencies]
frogql = { version = "0.5.11", default-features = false }
```

`default-features = false` drops the REPL and benchmark dependencies, so
your build pulls only `serde`, `serde_json` and `thiserror`.

`frogql::Connection` ships from 0.5.11. On an older version, depend on the
repository instead:
`frogql = { git = "https://github.com/pleiad/frogql", default-features = false }`.

## A complete program

```rust
use frogql::{Connection, Outcome, Value};

fn main() -> Result<(), frogql::Error> {
    // Creates the file if it does not exist, like sqlite3_open.
    let conn = Connection::open("people.gdb")?;

    // Writes: INSERT, SET, REMOVE, DELETE.
    if let Outcome::Modified(m) = conn.execute(
        "INSERT (:Person {name: 'Ada', born: 1815})-[:KNOWS]->(:Person {name: 'Alan', born: 1912})",
    )? {
        println!("{} nodes, {} edges inserted", m.nodes_inserted, m.edges_inserted);
    }

    // Writes live in memory until you save. save() is atomic.
    conn.save()?;

    // Reads.
    let rows = conn.query(
        "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a.name AS a, b.born AS born",
    )?;
    for row in rows.iter() {
        if let (Some(Value::Str(a)), Some(Value::Int(born))) = (row.get("a"), row.get("born")) {
            println!("{a} knows someone born in {born}");
        }
    }
    Ok(())
}
```

Output:

```
2 nodes, 1 edges inserted
Ada knows someone born in 1912
```

## The API

| Call | What it does |
|---|---|
| `Connection::open(path)` | Open a `.gdb`, or create an empty one. Builds the join index up front. |
| `conn.execute(stmt)` | Run any statement. Returns an `Outcome`. |
| `conn.execute_with_limit(stmt, n)` | Same, producing at most `n` rows (`0` = no cap). |
| `conn.query(stmt)` | Run a statement that returns rows. Returns `Rows`. |
| `conn.save()` | Write pending changes to the file. |
| `conn.node(id)` / `conn.edge(id)` | Labels and properties of an element. |
| `conn.edge_endpoints(id)` | Source and target node ids of an edge. |
| `conn.schema()` | The active graph type, as GQL text. |
| `conn.node_count()` / `conn.edge_count()` | Sizes. |
| `conn.set_timeout(Some(d))` | Stop queries that run longer than `d`. |
| `conn.set_cancel_flag(Some(flag))` | Stop the running query when another thread sets `flag`. |
| `frogql::connection::import_json(db, file)` | Build a `.gdb` from a JSON graph file. |
| `frogql::connection::import_json_str(db, json)` | Same, from a string in memory. |
| `frogql::connection::import_csv(db, dir)` | Build a `.gdb` from a CSV directory. |

`execute` returns one of three outcomes:

- `Outcome::Rows(Rows)` for a query, `SHOW ...` or `VALIDATE GRAPH TYPE`.
- `Outcome::Modified(Modified)` for DML, with insert, delete and modify counts.
- `Outcome::Message(String)` for `CREATE / USE / DROP GRAPH TYPE` and `CREATE / DROP INDEX`.

Errors come as `frogql::Error`: `Compile` (parse or type error), `Execution`,
`Io`, or `Interrupted` (timeout or cancel; no partial rows are returned).

## Rows and values

`Rows` holds `columns: Vec<String>` and `rows: Vec<Vec<Value>>`.
`rows.iter()` yields rows you can read by column name with `row.get("name")`.

Column names follow the bindings: the alias (`RETURN p.name AS name`), the
variable for a bare variable (`RETURN p`), and `col0`, `col1`, … otherwise.
A query with no `RETURN` has one column per pattern variable, sorted by name.

`Value` is an enum: `Null`, `Int`, `Float`, `Str`, `Bool`, `List`, `Record`,
`Date`, `LocalDatetime`, `Path`, and `Node(id)` / `Edge(id)`. A node or edge
comes back as an id. Resolve it with `conn.node(id)`:

```rust
let rows = conn.query("MATCH (p:Person) RETURN p")?;
for row in rows.iter() {
    if let Some(Value::Node(id)) = row.get("p") {
        let p = conn.node(*id);
        println!("{:?} {:?}", p.labels, p.props.get("name"));
    }
}
```

## Things to know

- **Save is explicit.** Mutations live in memory until `conn.save()`.
  Dropping the connection without saving discards them. `CREATE INDEX`
  works the same way.
- **One connection per thread.** `Connection` is neither `Send` nor `Sync`:
  open it on the thread that uses it. For a multi-threaded server, give one
  thread the connection and send it statements over a channel.
- **One writer per file.** Two processes writing the same `.gdb` overwrite
  each other at `save()`. There is no lock and no WAL yet.
- **`save()` renumbers ids.** Node and edge ids are internal and change on
  save. Use your own key property (`{uid: ...}`) to identify elements
  across sessions.
- **Sidecar file.** On first open, froGQL writes `<db>.ltj` beside the
  database: a cache of the join index. Deleting it is safe; it is rebuilt.
  Set `FROGQL_LTJ_PERSIST=0` to not write it.

## Bulk loading

For more than a few thousand elements, build a JSON graph in memory and
call `import_json_str` instead of running one `INSERT` per element:

```rust
let json = r#"{
  "nodes": [
    {"id": "a", "labels": ["City"], "props": {"name": "Lima"}},
    {"id": "b", "labels": ["City"], "props": {"name": "Quito"}}
  ],
  "edges": [
    {"id": "e1", "labels": ["ROAD"], "endpoints": ["a", "b"],
     "directionality": "->", "props": {"km": 1800}}
  ]
}"#;
frogql::connection::import_json_str("cities.gdb", json)?;
let conn = Connection::open("cities.gdb")?;
```

`directionality` is `"->"` for a directed edge or `"~~"` for an undirected
one. The import overwrites the destination file. See
[`data-import.md`](data-import.md) for the format and the CSV loader.

## Lower-level API

`Connection` wraps public pieces you can use directly:
`store::lazy::LazyGraphStore` (the file), `compile_query_with` (parse,
typecheck, optimize), `runtime::engine::Runtime` (run a compiled query)
and `runtime::dm::run_dm_with_index` (run a DML statement).
`conn.store()` returns the open `LazyGraphStore`. Read
`src/connection.rs` for how they fit together.

## What to read next

- [`frogql-for-agents.md`](frogql-for-agents.md): the query language in one file, including the Cypher habits that do not parse.
- [`data-modification.md`](data-modification.md): `INSERT`, `SET`, `REMOVE`, `DELETE`.
- [`graph-types.md`](graph-types.md): schemas and validation.
- [`secondary-indexes.md`](secondary-indexes.md): `CREATE INDEX`.
