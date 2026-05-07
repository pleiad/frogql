# Data Import

froGQL reads graph data from three formats. Every import path also runs
`infer_simple_schema` and persists the result as the `DEFAULT` GRAPH TYPE,
so the typechecker is useful immediately after the first open.

## CSV with config (Text2GQL / Cypher datasets)

```bash
frogql mydb.gdb --import-csv path/to/Spanner_Instance/
```

Reads `spanner_import_config.json` plus the CSV files it points to.

- Node CSVs: detected as files without `SRC_ID` / `DST_ID` columns; ID column
  resolved by trying `vid`, `<Label>_id`, any `*_id`, then column 0.
- Edge CSVs: `SRC_ID`, `DST_ID` plus property columns; edge label inferred from
  the config `label` field with known node-type prefixes stripped.
- All column lookups are case-insensitive.

## LDBC SNB CSV-Basic

```bash
frogql mydb.gdb --import-ldbc-csv path/to/social_network-sf0.1-CsvBasic-LongDateFormatter/
```

Three-pass loader:

1. Nodes — each LDBC entity owns its id space (`Place` id 0 ≠ `Organisation` id 0),
   so the loader keys node ids by `(entity_label, external_id)`.
2. Multi-valued attributes — files like `Person_email_emailaddress.csv`
   collapse onto the owning node as a `Value::List`.
3. Edges — directed by default, with property columns preserved.

## JSON

```bash
frogql mydb.gdb --import-json graph.json
```

```json
{
  "nodes": [
    { "id": "n1", "labels": ["Person"], "props": { "name": "Alice", "age": 30 } }
  ],
  "edges": [
    { "id": "e1", "labels": ["Knows"], "endpoints": ["n1", "n2"], "directionality": "->" }
  ]
}
```

After import, the `.gdb` file contains everything — graph data, string
table, label index, adjacency, and the `DEFAULT` GRAPH TYPE. The original
source files are no longer needed.
