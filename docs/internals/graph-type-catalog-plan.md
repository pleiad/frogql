# Plan: Catálogo persistente de GRAPH TYPE en gqlite

## Objetivo

Soportar un catálogo de GraphTypes nombrados, persistido en el `.gdb`, que
el typechecker consulte por sesión. La interfaz son cuatro sentencias DDL
estilo ISO GQL más una regla especial para el tipo `DEFAULT`.

## Sentencias

### `CREATE GRAPH TYPE <name> AS { <type-elements> };`

Define explícitamente un GraphType con type expressions. Lo persiste en el
catálogo del `.gdb` bajo `<name>`. No lo activa.

`<type-elements>` es una lista separada por `,` de:

- Nodos: `(:Label)`, `(:Label {prop STRING, prop2 INT})`, `(:A&B {x BOOL})`
- Edges dirigidos: `(:A)-[:E]->(:B)` o con props `(:A)-[:E {w FLOAT}]->(:B)`
- Edges no dirigidos: `(:A)~[:E]~(:B)`

Tipos primitivos (mayúsculas, alineado con ISO): `STRING`, `INT`, `INTEGER`,
`FLOAT`, `BOOL`, `BOOLEAN`. Internamente todos mapean a `SimpleType` existentes.
Aceptar también las variantes minúsculas como aliases.

Tipos compuestos en posición de propiedad (todos mapean a `SimpleType`
existentes en `src/typing/simple_type.rs`):

- Listas: `LIST<T>` o `[T]` como sufijo. Ej: `tags LIST<STRING>` o
  `tags [STRING]`. Anidables: `LIST<LIST<INT>>`.
- Records anidados: `{ k1 T1, k2 T2, ... }` como tipo de propiedad.
  Ej: `addr { city STRING, zip INT }`. Anidables: records de listas y
  viceversa.
- Uniones: `T1 | T2`. Ej: `id STRING | INT`. Asociativa.
- Wildcard: `ANY` (alias `*`) para `SimpleType::Star`. Ej: `payload ANY`.

Tipos compuestos en posición de label (LabelType existente):

- Conjunción: `:A&B` (multi-label). Ya soportado por el parser actual.
- Disyunción: `:A|B`. Ya soportado.
- Negación: `:!A`. Ya soportado.
- Wildcard sin label: `()` o `(:)` permite cualquier label.

Cuerpo es closed/required por defecto: solo las props listadas son válidas
y todas son obligatorias. Si se decide soportar opcionales en esta iteración,
sufijo `?` después del tipo: `{name STRING, nick STRING?}`.

### `USE GRAPH TYPE <name>;`

Activa `<name>` como schema del typechecker. Persiste el cambio de "active
type" en el `.gdb` para que sobreviva a cierre y reapertura.

### `USE GRAPH TYPE DEFAULT;`

Caso especial. `DEFAULT` es un nombre reservado que siempre representa el
schema simplificado inferido desde los datos actuales. Cada ejecución:

1. Llama a `infer_simple_schema(&store)` (lógica heredada de
   `print_schema_simple`).
2. Sobreescribe el GraphType `DEFAULT` en el catálogo persistido.
3. Lo activa.

Idempotente. Refleja siempre el estado vigente del grafo.

### `DROP GRAPH TYPE <name>;`

Elimina `<name>` del catálogo. Si era el activo, vuelve a `Schema::star()`
(modo permisivo, equivalente a no tener type activo).

`DROP GRAPH TYPE DEFAULT` se rechaza con error explícito:
`"DEFAULT is a reserved graph type and cannot be dropped"`. Sí se puede
regenerar con `USE GRAPH TYPE DEFAULT` cuantas veces se quiera.

## Reglas del nombre reservado `DEFAULT`

- `CREATE GRAPH TYPE DEFAULT AS { ... }` se rechaza en parser/validación.
- `DROP GRAPH TYPE DEFAULT` se rechaza.
- `DEFAULT` solo se crea/refresca vía `USE GRAPH TYPE DEFAULT` o vía la
  inferencia automática al importar.
- Cuando hay conflicto de nombre con un type custom previo (caso imposible
  por la regla anterior), gana DEFAULT.

## Inferencia automática al importar

Al ejecutar `gqlite db.gdb --import-csv <dir>` o `--import-json <file>`:

1. Cargar datos como hasta ahora (`csv_loader::load_from_csv_dir` o
   `Graph::from_file`).
2. Antes de cerrar el archivo guardado, llamar a `infer_simple_schema(&graph)`
   y persistirlo en el catálogo bajo `DEFAULT`.
3. Marcar `DEFAULT` como el activo del nuevo `.gdb`.
4. Reportar en la salida del import: `"Inferred DEFAULT graph type:
   N node types, M edge types"`.

`.gdb` viejos sin catálogo se abren igual que antes. El schema activo es
`Schema::star()` hasta que el usuario corra `USE GRAPH TYPE DEFAULT` o
defina uno propio.

## Persistencia: cambios en el formato `.gdb`

### Header (`src/pager/header.rs`)

Hoy el header tiene "reservado (zeroed)" desde el byte 60. Reservar:

```
bytes 60-63: catalog root page (u32 LE, 0 = sin catálogo)
bytes 64-95: active type name (u8[32], null-terminated, vacío = sin activo)
bytes 96+:   reservado
```

Limitar el nombre activo a 32 bytes UTF-8 simplifica el header. Nombres más
largos en `<name>` son válidos para CREATE, pero el activo se trunca con
warning si excede 31 bytes (caso muy improbable en práctica).

`FORMAT_VERSION` no necesita bump: archivos viejos ya tienen ceros en estos
offsets, lo cual se interpreta correctamente como "no hay catálogo, no hay
activo". Migración cero.

### Página de catálogo

Una sola página apunta al inicio del catálogo serializado. El payload puede
exceder una página; se encadena con un puntero "next page" en los primeros
4 bytes del área de datos de cada página:

```
[u32 next_page (LE)] [bytes de payload]
[u32 next_page (LE)] [bytes de payload]
...
[u32 0]              [últimos bytes]
```

### Formato del payload

JSON via `serde_json` sobre estructuras del módulo `typing/`. Razones:
catálogos son chicos (decenas de KB max), debugeable, `serde` ya está como
dependencia, evita agregar `bincode`. Si en el futuro hay catálogos grandes
se puede migrar a binario sin tocar la API pública.

```json
{
  "version": 1,
  "types": {
    "DEFAULT": { "nodes": [...], "edges": [...] },
    "strict":  { "nodes": [...], "edges": [...] }
  }
}
```

Cada `Schema` se serializa con `#[derive(Serialize, Deserialize)]` agregado
a `Schema`, `VariableType`, `DescriptorType`, `LabelType`, `PropertyType`,
`SimpleType`. Si alguna variante tiene tipos no-serializables, ajustar
ad-hoc; en principio toda la jerarquía actual es serializable directo.

### API de storage

Nuevo módulo `src/store/catalog_io.rs`:

```rust
pub fn read_catalog(pager: &mut Pager, root_page: u32)
    -> io::Result<GraphTypeCatalog>;
pub fn write_catalog(pager: &mut Pager, catalog: &GraphTypeCatalog)
    -> io::Result<u32>;       // returns new root page
```

Todas las rutas de open (`Graph::open`, `LazyGraphStore::open`,
`DiskGraphStore::open`) leen el catálogo si `header.catalog_root != 0`,
si no, devuelven catálogo vacío con `active = None`.

Todas las rutas de save (`Graph::save`, vía `store/io.rs`) reescriben el
catálogo. La activación se persiste en el header en cada DDL relevante:
`CREATE`, `USE`, `DROP`. El runtime tiene que poder mutar el `.gdb`
abierto, no solo leerlo. Para esta iteración: en el REPL, tras cada DDL,
re-abrir el `Pager` en modo write, escribir catálogo + header, cerrar.
Para los bindings de Python lo mismo en `Connection.execute`. Si reescribir
el header en cada DDL resulta complicado con la abstracción actual, dejar
el activo en memoria y persistir solo en `Connection.close()` o en un
`save_catalog()` explícito, marcando como follow-up.

## Catálogo en runtime

`src/runtime/catalog.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphTypeCatalog {
    pub types: BTreeMap<String, Schema>,
    pub active: Option<String>,
}

impl GraphTypeCatalog {
    pub fn new() -> Self;
    pub fn register(&mut self, name: String, schema: Schema)
        -> Result<(), String>;        // rechaza "DEFAULT"
    pub fn install_default(&mut self, schema: Schema);
                                       // sobreescribe DEFAULT y lo activa
    pub fn drop(&mut self, name: &str) -> Result<(), String>;
                                       // rechaza "DEFAULT"
    pub fn set_active(&mut self, name: &str) -> Result<(), String>;
    pub fn clear_active(&mut self);
    pub fn active_schema(&self) -> Schema;  // clone del activo o Schema::star()
    pub fn list(&self) -> Vec<(&String, bool)>;  // (nombre, is_active)
}
```

`Runtime` recibe (o referencia) un catálogo. Cada `compile_query` interno
usa `catalog.active_schema()`.

## Inferencia (extraer de print_schema_simple)

Nuevo módulo `src/typing/inference.rs`:

```rust
pub fn infer_simple_schema<G: GraphAccess>(g: &G) -> Schema;
```

Reusar la lógica de agrupación de `print_schema_simple` (gqlite.rs:512).
Cada grupo se convierte a:

- Nodo: `VariableType::Node(DescriptorType { labels: LabelType::and(...),
  props: PropertyType::closed_or_open(...) })`. Open si el grupo tenía
  `has_optional`, closed si no.
- Arista: `VariableType::EdgeDirectional` o `EdgeNonDirectional` según
  `directed`, con `left`/`right` apuntando a los Node correspondientes.

`print_schema_simple` se refactoriza para llamar a `infer_simple_schema` y
formatear el resultado.

## Compilación con schema custom

En `src/lib.rs`:

```rust
pub fn compile_query_with(schema: &Schema, input: &str) -> Result<Query, String>;
```

Idéntica a `compile_query` pero pasando `Typechecker::new(schema.clone())`.
`compile_query` se mantiene tal cual.

## Parser

```rust
pub enum Statement {
    Query(Query),
    CreateGraphType { name: String, body: Vec<TypeElement> },
    UseGraphType { name: String, refresh_default: bool },
    DropGraphType { name: String },
}

pub enum TypeElement {
    Node(VariableType),
    Edge(VariableType),
}
```

Nuevo entry point `parser::parse_statement(input) -> Result<Statement, String>`.
`parse_query` se mantiene y rechaza DDL.

Validaciones del parser:

- `CREATE GRAPH TYPE DEFAULT AS { ... }` produce error
  `"DEFAULT is a reserved graph type name"`.
- `DROP GRAPH TYPE DEFAULT` produce el error correspondiente.
- `USE GRAPH TYPE <id>` setea `refresh_default = (id.to_uppercase() == "DEFAULT")`.

Keywords nuevos: `CREATE`, `USE`, `DROP`, `GRAPH`, `TYPE`, `AS`, `DEFAULT`.
Tipos primitivos en mayúscula como aliases en posición de schema body.

## REPL (`src/bin/gqlite.rs`)

- Reemplazar la llamada a `compile_query` por `parse_statement`.
- Despachar:
  - `Query`: `compile_query_with(catalog.active_schema(), ...)` y ejecutar.
  - `CreateGraphType`: construir `Schema` desde los `TypeElement`,
    `catalog.register(name, schema)?`, persistir, feedback
    `"GRAPH TYPE '<name>' created (<N> node types, <M> edge types)."`.
  - `UseGraphType { refresh_default: true }`: `infer_simple_schema(store)`,
    `catalog.install_default(s)`, persistir, feedback
    `"GRAPH TYPE 'DEFAULT' refreshed (<N> node types, <M> edge types) and activated."`.
  - `UseGraphType { refresh_default: false }`: `catalog.set_active(&name)?`,
    persistir, feedback `"Active GRAPH TYPE: <name>."`.
  - `DropGraphType`: `catalog.drop(&name)?`, persistir, feedback
    `"GRAPH TYPE '<name>' dropped."`.
- Mantener `schema` y `schema simple` como meta-comandos de display.
- Agregar meta-comando `:graph-types` que liste con `*` el activo.

## Bindings Python (`python/src/lib.rs`)

- `Connection` embebe `RefCell<GraphTypeCatalog>` y maneja persistencia
  igual que el REPL.
- `Connection.execute` rutea con `parse_statement`. DDL devuelve
  `{"ok": true, "kind": "ddl", "message": "..."}`. Query devuelve filas
  como ahora pero usando `catalog.active_schema()`.
- Nuevo método `Connection.graph_types() -> list[dict]`:
  `[{"name": "DEFAULT", "active": true, "nodes": N, "edges": M}, ...]`.

## Importación automática (`src/bin/gqlite.rs::import`)

Tras `graph.save(db_path)`:

```rust
let schema = infer_simple_schema(&graph);
let mut catalog = GraphTypeCatalog::new();
catalog.install_default(schema);
catalog::write_to_db(db_path, &catalog)?;
eprintln!("Inferred DEFAULT graph type: {} node types, {} edge types",
    n, m);
```

## Tests

Nuevo `tests/graph_type_test.rs`:

### Parser
- Acepta `CREATE GRAPH TYPE foo AS { (:Person {name STRING}) }`.
- Acepta `CREATE GRAPH TYPE foo AS { (:A)-[:E {w FLOAT}]->(:B), (:A) }`.
- Acepta `STRING`, `INT`, `INTEGER`, `FLOAT`, `BOOL`, `BOOLEAN`, mayúsculas
  y minúsculas.
- Rechaza `CREATE GRAPH TYPE DEFAULT AS { ... }` con mensaje específico.
- Rechaza `DROP GRAPH TYPE DEFAULT`.
- Acepta `USE GRAPH TYPE foo`, `USE GRAPH TYPE DEFAULT`, `DROP GRAPH TYPE foo`.

### Parser, tipos compuestos en propiedades
- Listas planas: `CREATE GRAPH TYPE g AS { (:Post {tags LIST<STRING>}) }`.
- Sintaxis sufijo equivalente: `{tags [STRING]}`. Genera el mismo
  `SimpleType::List(Box::new(SimpleType::S))`.
- Listas anidadas: `{matrix LIST<LIST<INT>>}`.
- Records anidados como tipo de propiedad:
  `{ addr { city STRING, zip INT } }` produce
  `SimpleType::Record(BTreeMap{city -> S, zip -> Z})`.
- Records de listas: `{ history { events LIST<STRING>, total INT } }`.
- Listas de records: `{ logs LIST<{ ts INT, msg STRING }> }`.
- Uniones en posición de propiedad: `{ id STRING | INT }` produce
  `SimpleType::Union(S, Z)`. Asociativa: `STRING | INT | BOOL` parsea
  como `Union(S, Union(Z, B))` o `Union(Union(S, Z), B)` (decidir y
  fijar; ambas son equivalentes vía la `union` simétrica del lattice).
- Wildcard: `{ payload ANY }` y `{ payload * }` producen
  `SimpleType::Star`.
- Mezcla profunda: `{ doc { tags LIST<STRING | INT>, meta ANY } }`.

### Parser, tipos compuestos en labels
- Conjunción de labels en nodos: `(:Person&Employee {salary INT})`
  produce un Node con `LabelType::and([Person, Employee])`.
- Disyunción de labels: `(:Customer|Vendor)` produce
  `LabelType::or([Customer, Vendor])`.
- Negación de labels: `(:!Internal)` produce `LabelType::not(Internal)`.
- Label expression compuesta: `(:A&B|C)` (verificar precedencia documentada
  del parser actual y respetarla).
- Conjunción en labels de edge: `(:A)-[:E1&E2 {w FLOAT}]->(:B)`.

### Comportamiento end-to-end
- `CREATE GRAPH TYPE g AS { (:Person {name STRING}) }; USE GRAPH TYPE g;`
  hace que `MATCH (x: Foo) RETURN x` falle en typecheck pero
  `MATCH (x: Person) RETURN x.name` pase.
- `USE GRAPH TYPE DEFAULT` sin contexto previo construye + activa.
- `DROP GRAPH TYPE g` quita y vuelve a star: `MATCH (x: Foo)` vuelve a pasar.
- `DROP GRAPH TYPE DEFAULT` retorna error.

### Comportamiento end-to-end, tipos compuestos
Sobre un grafo de prueba con nodos `Post {title: str, tags: [str]}`,
`User&Admin {name: str, level: int}`, y aristas `(User)-[:WROTE]->(Post)`:

- Listas en propiedades: con
  `CREATE GRAPH TYPE g AS { (:Post {title STRING, tags LIST<STRING>}) }`
  activo, `MATCH (p: Post) RETURN p.tags` pasa typecheck;
  `MATCH (p: Post) WHERE p.tags is INT RETURN p` falla.
- Listas anidadas: tras inferir DEFAULT sobre datos con
  `matrix: [[1,2],[3,4]]`, el tipo de `matrix` debe ser
  `SimpleType::List(List(Z))`.
- Records anidados: con
  `CREATE GRAPH TYPE g AS { (:User {addr { city STRING, zip INT }}) }`,
  query `MATCH (u: User) RETURN u.addr` pasa typecheck con
  `SimpleType::Record(...)`; acceso a campo `u.addr.city` también pasa.
- Multi-label conjunción:
  `CREATE GRAPH TYPE g AS { (:User&Admin {name STRING, level INT}) }`
  con `USE GRAPH TYPE g` activo: `MATCH (u: User&Admin) RETURN u.level`
  pasa; `MATCH (u: User) RETURN u.level` también pasa (User es supertipo
  consistente con la conjunción); `MATCH (u: Admin) WHERE u.level is STRING`
  falla.
- Disyunción de labels en query contra schema con conjunción:
  con `(:User&Admin)` en el schema, `MATCH (x: User|Guest)` pasa
  typecheck para la rama User pero el resultado es vacío en runtime si
  no hay nodos Guest. Verificar que el meet se computa sin error.
- Edge con labels compuestos:
  `CREATE GRAPH TYPE g AS { (:User)-[:WROTE&MAIN]->(:Post) }` lleva a
  que solo aristas con ambas labels matcheen.
- Unión de tipos en propiedad: schema
  `(:Item {id STRING | INT})`. Una query con `MATCH (i: Item) WHERE i.id is STRING`
  pasa; con `WHERE i.id is BOOL` falla.
- Wildcard: schema `(:Doc {payload ANY})` permite cualquier tipo en
  `payload` (`is INT`, `is STRING`, lista, record, todo pasa typecheck).
- Round-trip de inferencia: cargar un grafo con propiedades de tipo
  lista y record, llamar a `infer_simple_schema(&graph)`, y verificar
  que el `Schema` resultante contiene `SimpleType::List(...)` y
  `SimpleType::Record(...)` en las posiciones correctas.
- Round-trip de serialización: serializar a JSON un Schema que contenga
  cada uno de los compuestos (lista, record, unión, conjunción de labels,
  wildcard, anidación profunda), parsearlo de vuelta, comparar con
  `assert_eq!`. Cubre la implementación de `Serialize`/`Deserialize`
  sobre toda la jerarquía.

### Persistencia
- Round-trip básico: crear catálogo, escribirlo a un `.gdb` temporal,
  reabrirlo, comparar.
- Tras `CREATE GRAPH TYPE g; USE GRAPH TYPE g;`, cerrar y reabrir el `.gdb`:
  `g` sigue activo y queries siguen typechecking contra él.
- Tras `DROP GRAPH TYPE g`, cerrar y reabrir: `g` no aparece y schema activo
  está vacío (star).

### Importación
- `gqlite tmp.gdb --import-json examples/movies.json` (o equivalente)
  resulta en un `.gdb` con `DEFAULT` poblado y activo.
- `infer_simple_schema(graph)` da los mismos labels y prop sets que la
  agrupación previa de `print_schema_simple` sobre `examples/movies.gdb`.

### Backward compatibility
- Abrir un `.gdb` legacy (catalog_root = 0): catálogo vacío, schema activo
  es star, queries funcionan como antes.
- Tras una sesión que crea un GraphType y cierra, el archivo legacy queda
  con catálogo poblado; abrirlo con un binario viejo (compilado antes del
  cambio) sigue funcionando porque los offsets antiguos no se modifican.

## Restricciones de la iteración

- Solo `DEFAULT` se crea automáticamente al importar. Otros tipos siempre
  vienen del usuario via DDL.
- Si las props opcionales con `?` resultan más trabajo del previsto, dejar
  todo el cuerpo de CREATE como closed/required y marcar como follow-up.
- Catálogos grandes (>1 MB) no son objetivo. Si el JSON serializado supera
  algún umbral razonable, fallar explícitamente en lugar de fragmentar
  agresivamente.
- No invalidar `DEFAULT` automáticamente cuando los datos cambian post-import.
  El usuario debe correr `USE GRAPH TYPE DEFAULT` para regenerar (en este
  prototipo no hay mutación in-place de `.gdb`, así que el caso solo aplica
  si alguien reimporta sobre un archivo existente).

## Criterios de éxito

1. Build limpio, sin warnings nuevos.
2. Suite completa pasa, incluyendo `graph_type_test`:
   ```
   cargo test --release \
     --test parser_test --test runtime_test --test store_runtime_test \
     --test text2gql_test --test parse_and_run_test \
     --test typecheck_test --test typecheck_smoke \
     --test elaborate_test --test float_test --test list_test --test record_test \
     --test graph_type_test
   ```
3. Sesión REPL ejemplo:
   ```
   $ gqlite movies.gdb --import-csv movies_csv/
   ... loading ...
   Inferred DEFAULT graph type: 4 node types, 6 edge types
   $ gqlite movies.gdb
   Active GRAPH TYPE: DEFAULT.
   gql> MATCH (x: Person) RETURN x.name;
   ... rows ...
   gql> MATCH (x: Foo) RETURN x.bar;
   typecheck error: label Foo not in active schema
   gql> CREATE GRAPH TYPE strict AS { (:Person {name STRING}) };
   GRAPH TYPE 'strict' created (1 node types, 0 edge types).
   gql> USE GRAPH TYPE strict;
   Active GRAPH TYPE: strict.
   gql> :graph-types
     DEFAULT
   * strict
   gql> DROP GRAPH TYPE DEFAULT;
   error: DEFAULT is a reserved graph type and cannot be dropped
   gql> quit
   $ gqlite movies.gdb
   Active GRAPH TYPE: strict.
   ```

## Follow-ups (fuera de alcance)

- Forma estándar `CREATE GRAPH g TYPED foo` para atar un grafo a un type al
  crearlo (ahora todo grafo es schema-free hasta que el usuario active uno).
- Migración de formato: si en algún momento se cambia la serialización del
  catálogo, agregar un `format_version` en el header del catálogo y bump.
- Invalidación automática de `DEFAULT` cuando cambian los datos.
- Constraints de unicidad / cardinalidad estilo Neo4j 5.x dentro del cuerpo
  de `CREATE GRAPH TYPE`.
