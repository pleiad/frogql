# ISO GQL: qué falta en froGQL

Este documento lista lo que falta en froGQL respecto al estándar ISO/IEC 39075:2024, ordenado por impacto en las queries que los usuarios efectivamente escriben. Sirve como roadmap de features pendientes; lo ya implementado vive bajo "Estado actual".

## Estado actual (cierre de MVP-1)

Implementado y cubierto por tests:

- **Parser y query language**: `MATCH`, `OPTIONAL MATCH`, `WHERE`, `RETURN` (con `DISTINCT` y alias `AS`), `ORDER BY ... ASC|DESC NULLS FIRST|LAST`, `LIMIT`, `GROUP BY`, comma-join.
- **Expresiones de valor (cierre IC7)**: división `/` (ISO `<solidus>`), `FLOOR(<numeric>)`, `CAST(<op> AS INTEGER|FLOAT)` (conversión de valor, distinta de la aserción de tipo `AS`), constructor de record `RECORD { k: <expr>, ... }` con valores-expresión (`RECORD` opcional, fast-path constante), y la subconsulta de valor `VALUE { MATCH ... RETURN <1 item> ORDER BY ... LIMIT 1 }` (correlacionada, arg-max por grupo). `FLOOR`/`CAST`/`RECORD`/`VALUE` son soft keywords (solo antes de `(`/`{`). `GROUP BY <binding variable>` agrupa por identidad de nodo/arista con chequeo de dependencia funcional ISO §14; `ORDER BY <alias>.<campo>` resuelve campos de columnas proyectadas tipo record (`SortKey::ColumnField`). Estas seis primitivas desbloquearon **IC7** (`bench/ldbc-queries/ic7.toml`, `tests/ic7_test.rs`).
- **Path patterns**: concat, union (`|`), filter (`WHERE`), repetición `{n,m}`, optional (`?`), aristas dirigidas, reversas y no dirigidas, labels conjuntivas (`A & B`), disyuntivas (`A | B`) y negadas (`!A`).
- **Named paths y path functions (§16.6 + §20.16)**: declaración `MATCH path = (...)` (binding de un operando a una variable-camino, fuera del prefijo §16.6), `Value::Path` proyectable, y las funciones `ELEMENTS` / `PATH_LENGTH` / `CARDINALITY` (conformes) más `NODES` / `EDGES` (divergencia de traducción). El typechecker liga la variable-camino y valida que el argumento de cada función tipe como camino.
- **Path-pattern prefixes (ISO §16.6)**: path modes `WALK` / `TRAIL` / `SIMPLE` / `ACYCLIC` y path searches `ALL` / `ANY [N]` / `SHORTEST [N] [PATHS]` / `SHORTEST N GROUPS` (con las formas normalizadas `ANY SHORTEST` y `ALL SHORTEST`). Habilitan repetición ilimitada (`*`, `+`) vía búsqueda k-shortest sobre walks o enumeración finita podada por modo. Aislamiento §16.6 SR 5–8 verificado en el typechecker.
- **Tipos y valores**: `Int`, `Float`, `Str`, `Bool`, `List`, `Record` (anidable), `Null` con lógica trivalente. `Value::Node` y `Value::Edge` como reference values de primera clase.
- **Predicados existenciales**: `EXISTS { ... }` y `NOT EXISTS { ... }` con correlación, fold a literal cuando el body es trivialmente vacío.
- **Aggregation (Feature GF10 parcial)**: `COUNT(*)`, `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `COLLECT_LIST` (alias `COLLECT` / `ARRAY_AGG`). Null elimination automática y agregados vacíos que producen `null`. `COLLECT_LIST` arma un `Value::List` por grupo y dropea records all-null (lado vacío de un OPTIONAL). Aritmética sobre agregados en la proyección (`COUNT(DISTINCT x) + COUNT(DISTINCT y) AS total`): un agregado puede ser operando de un `Binop`, evaluado por grupo tras el `GROUP BY`.
- **DML (ISO §13)**: `INSERT`, `SET x.prop = expr`, `SET x = { ... }` (clear+set), `SET x:Label`, `REMOVE x.prop`, `REMOVE x:Label`, `[DETACH | NODETACH] DELETE <expr list>`, `RETURN` post-DM. Validación G2000 contra el GRAPH TYPE activo, atomicidad por statement vía overlay.
- **DDL de catálogo**: `CREATE / USE / DROP / SHOW / VALIDATE GRAPH TYPE`, `CREATE / DROP / SHOW INDEX` (HASH y BTREE).
- **Storage**: archivo único `.gdb` con páginas de 4KB, catálogo persistido, atomicidad de `.save` vía tmp+rename, dumps `.dump-json` y `.dump-gql`.
- **Optimizer**: predicate pushdown, label index selection, Leapfrog Triejoin con índices secundarios.

## Pendiente

### Tier 1: bloqueadores reales (vacío)

Las cuatro features que quedaban en Tier 1 (aggregation, `NOT EXISTS`, `OPTIONAL MATCH`, `Null` con 3VL) ya están en `main`. Lo que sigue son features de calidad de vida.

### Tier 2: útiles, hay workaround

#### 2.1 WITH / NEXT / LET (pipelines)

ISO usa `NEXT` o `LET` para encadenar etapas; Cypher las llama `WITH`. Permite filtrar y reagrupar después de un `MATCH` sin recurrir a subqueries:

```
MATCH (a: Person)-[:ACTED_IN]->(m)
NEXT WITH a, COUNT(m) AS c
WHERE c > 10
MATCH (a)-[:FOLLOWS]->(f)
RETURN a.name, f.name
```

Es la pieza más reclamada, porque sin ella no se puede filtrar sobre el resultado de una agregación. Toca el AST (nuevo nodo `Pipeline`), el typechecker (la siguiente etapa hereda el binding table de la anterior) y el runtime (rebind del working table entre etapas).

#### 2.2 OFFSET / SKIP

`LIMIT` ya está en la sintaxis. Falta saltar las primeras N filas tras `ORDER BY`. Cambio acotado al post-procesado del proyectado en `Runtime::run_query`.

#### 2.3 UNWIND

Inverso de `COLLECT`: dada una lista, emite una fila por elemento.

```
UNWIND [1, 2, 3] AS x
RETURN x * 10
```

Natural ahora que `List` es tipo de primera clase y que tenemos `WITH`-equivalente en el roadmap.

#### 2.4 Shortest path — implementado (ISO §16.6)

Cubierto por los path-pattern prefixes: `SHORTEST [N] [PATHS]`, `SHORTEST N GROUPS`, `ANY SHORTEST`, `ALL SHORTEST`, más los modos `TRAIL` / `SIMPLE` / `ACYCLIC` que hacen finita la repetición ilimitada. El runtime usa una búsqueda k-shortest sobre walks (`run_repetition_shortest`, heap ordenado por longitud con presupuesto por par `(origen,destino)`) o enumeración podada por modo (`run_repetition_unbounded_mode`); no decompone a triples LTJ. Ver `src/runtime/path_select.rs` y la sección *Path-pattern prefixes* del CLAUDE.md de la crate. Pendiente de §16.6: las funciones nombradas estilo Cypher `shortestPath()` / `allShortestPaths()` (la sintaxis de prefijo ISO cubre los mismos casos).

#### 2.5 Funciones built-in

Existe `Expr::Call { name, args }` con dispatch en `engine.rs::eval_call`; hoy resuelve `FLOOR` y `CAST` (más `COALESCE`/`DURATION` por Token dedicado) y las path-functions `ELEMENTS` / `PATH_LENGTH` / `CARDINALITY` / `NODES` / `EDGES` (ver 2.9). Faltan `size(list)`, `head`, `tail`, `type(edge)`, `labels(node)`, `EXTRACT(<part> FROM <date>)` y el operador `MOD`. Agregar cada uno es un arm nuevo en `eval_call` + `check_expr`.

#### 2.6 COLLECT y STDDEV (Feature GF10 completa)

Las agregaciones que faltan respecto a la lista del estándar. `COLLECT_LIST(x)` arma un `Value::List` por grupo; `STDDEV` y `STDDEV_POP` son aritméticas. Encajan en la misma infraestructura `GeneralSetKind` que `SUM`/`AVG`.

#### 2.7 Multi-DML chains en un solo statement

ISO §13.1 permite `MATCH α INSERT β SET γ MATCH δ DELETE ε` como una sola "linear data-modifying statement". El parser actual acepta a lo más un op DML por statement. Los tests de MVP-1.D actualmente parten en dos llamadas a `run_dm` lo que en ISO sería un único statement.

#### 2.8 String escapes en literales

El lexer no admite escapes (`'don\'t'` no parsea, `''` se lee como string vacío). Bloquea `.dump-gql` para nodos cuyas propiedades string contengan `'`. Fix conceptualmente trivial; toca `Lexer::tokenize` y la simétrica en `format_gql_value`.

#### 2.9 Named path patterns y path functions — implementado (2026-06-03)

`MATCH path = (a)-[:k]->(b)` liga el patrón completo a una variable-camino. La declaración ISO `<path variable declaration> ::= <binding variable> =` se parsea por operando de comma-join, fuera del prefijo §16.6 (`Named { var, Selected { prefix, pattern } }`), y materializa un `Value::Path` proyectable. Las path-functions de §20.16 operan sobre él:

- `ELEMENTS(path)` — lista de todos los elementos (nodos y aristas) en orden de match (conforme ISO).
- `PATH_LENGTH(path)` — número de aristas (conforme ISO).
- `CARDINALITY(path)` — número total de elementos, nodos más aristas (conforme ISO).
- `NODES(path)` / `EDGES(path)` — proyecciones nodo-solo / arista-solo. **No** son ISO (§20.16 no las define); se ofrecen como **divergencia de traducción** para los queries LDBC que las usan. Anótalas como divergencia en los toml afectados.

Implementación: variantes `PathPattern::Named`, `PathValue::Path`, `Value::Path`, `SimpleType::Path`, `VariableType::Path` (terminal en el retículo: solo se encuentra consigo misma, nunca refina contra schema, nunca vacía un entorno). El typechecker liga la variable-camino en el `TypeEnvironment` y exige que el argumento de cada path-function tipe como `Path` (un argumento demostrablemente no-camino es error de tipo). El runtime captura la secuencia ya construida en `ResultRow.paths` (no recomputa). Tests en `tests/named_path_test.rs`. Desbloqueó el prerequisito cruzado de IC1, IC13 e IC14 (cada uno con un gap adicional propio: `COLLECT_LIST`, `CASE WHEN`, list comprehension).

#### 2.10 CASE WHEN, EXTRACT, MOD, list comprehension

Expresiones de valor ISO que aún no parsean:

- `CASE WHEN <cond> THEN <e> ELSE <e> END` (`<case expression>`) — bloquea IC10, IC13. Nuevo `Expr::Case`.
- `EXTRACT(<field> FROM <datetime>)` (`<extract expression>`) y el operador `MOD` (`<modulo>`) — bloquean IC10.
- List comprehension `[x IN <list> | <expr>]` (`<list value constructor by enumeration>` con filtro/map) — bloquea IC14. Nuevo `Expr::ListComprehension`.

#### 2.11 COLLECT_LIST / multiset — implementado (2026-06-03)

`COLLECT_LIST(x)` (alias `COLLECT` / `ARRAY_AGG`) arma un `Value::List` por grupo. Es un `GeneralSetKind::CollectList` cuyo reducer en `apply_aggregator` envuelve los valores ya recolectados (con eliminación de nulls y `DISTINCT` heredados de `collect_aggregate_values`) en `Value::List`. Tipa como `List(elem)`. Además dropea records all-null, que vienen del lado vacío de un `OPTIONAL MATCH` (`RECORD { a: opt.x }` con `opt` sin match → todos los campos null) y representan "sin fila". Tests en `tests/collect_list_test.rs`.

Necesario para IC1 e IC12, pero **no suficiente**: ambos además agrupan y ordenan por *alias* de RETURN, no por variable de binding (ver 2.12).

#### 2.12 GROUP BY / ORDER BY por alias de RETURN

`GROUP BY <binding variable>` ya funciona (agrupa por identidad de nodo/arista). Lo que falta es `GROUP BY <alias>` donde `<alias>` es un nombre de columna de RETURN (`friend.id AS friendId ... GROUP BY friendId`), más alias dentro de expresiones en `ORDER BY` (`ORDER BY CAST(friendId AS INTEGER)`). El typechecker rechaza el alias con "Variable friendId not found in context" porque no es una variable de binding. IC1 e IC12 dependen de esto (sus GROUP BY listan `friendId, friendLastName, distanceFromPerson, ...`, todos aliases). Es resolución de nombres: una pre-pasada de elaboración que sustituye `Expr::Var(alias)` en GROUP BY y dentro de exprs de ORDER BY por la expresión aliaseada del RETURN, con cuidado del shadowing alias-vs-variable. `ORDER BY <alias>` a secas ya lo resuelve `order_by_alias.rs`; falta el caso GROUP BY y el alias-dentro-de-expr.

### Tier 3: producción, no investigación

#### 3.1 Transacciones reales (WAL, recovery)

Hoy `.save` es la única primitiva de "commit"; entre saves la sesión acumula RAM proporcional al overlay. Un crash entre saves pierde toda la mutación posterior al último save. Una WAL real exigiría dirty page tracking, recovery loop al open, y locking inter-conexión. Trabajo de varias semanas, sin demanda en research.

#### 3.2 Concurrencia inter-sesión

`Connection` es `unsendable` en Python y la implementación es single-threaded por design. Habilitar lectores concurrentes pide MVCC sobre el page cache; escritores concurrentes piden además WAL.

#### 3.3 Indexes incrementales bajo overlay

Cuando hay overlay no vacío, `lookup_node_eq` y `lookup_node_range` retornan `None` y el caller hace scan. Mantener hash y btree incrementales durante DML cuesta O(log N) por mutación; cabe en MVP-2.

## Cobertura LDBC Interactive Complex (IC)

Estado de los 14 IC del benchmark cross-system (`bench/ldbc-queries/ic*.toml`). "Implementado" = el query corre por el motor y produce filas; la verificación de equivalencia de filas vive en `bench/cross-system/`.

| IC | Estado | Gaps restantes |
|----|--------|----------------|
| IC2, IC3, IC4, IC5, IC6, IC8, IC9, IC11 | implementado | — |
| **IC7** | **implementado** | — (necesitaba `VALUE`, `RECORD`, `CAST`, `FLOOR`, `/`, `GROUP BY <var>`; todos hechos) |
| IC1 | blocked | GROUP BY / ORDER BY por alias (2.12). Named paths + `PATH_LENGTH`/`NODES` ✅ (2.9), `RECORD` ✅, `ANY SHORTEST` ✅, `COLLECT_LIST` ✅ (2.11) |
| IC10 | blocked | `EXTRACT(... FROM ...)`, `MOD`, `CASE WHEN` (2.10) |
| IC12 | blocked | GROUP BY / ORDER BY por alias (2.12). `COLLECT_LIST` ✅ (2.11); `[:isSubclassOf]->{0,}` ya parsea y el typechecker lo admite bajo un prefijo de modo (`ACYCLIC`/`TRAIL`) — divergencia de traducción, sin código nuevo |
| IC13 | blocked | `CASE WHEN` (2.10). Named paths + `PATH_LENGTH` ✅ (2.9) |
| IC14 | blocked | list comprehension (2.10). Named paths + `NODES` ✅ (2.9), `VALUE` ✅, `*` ✅ |

### Roadmap para los 14 IC completos

Ordenado por leverage (ICs desbloqueados por feature):

1. ~~**Named path patterns + path functions** (2.9)~~ — **hecho (2026-06-03)**. `MATCH path = [ANY|ALL] SHORTEST (...)`, `ELEMENTS`, `PATH_LENGTH`, `CARDINALITY`, más `NODES`/`EDGES` (divergencia). Desbloqueó el prerequisito de **IC1, IC13, IC14**.
2. ~~**`COLLECT_LIST` / multiset aggregate** (2.11)~~ — **hecho (2026-06-03)**. `GeneralSetKind::CollectList`, reducer a `Value::List`, drop de records all-null. Era prerequisito de **IC1 e IC12**, pero ninguno cierra sin (3).
3. **GROUP BY / ORDER BY por alias** (2.12) — cierra **IC1 e IC12** (con la divergencia `{0,}`→prefijo en IC12). Resolución de nombres en una pre-pasada de elaboración.
4. **`CASE WHEN ... END`** (2.10) — cierra **IC13** y parte de **IC10**. `Expr::Case` + typecheck del join de ramas.
5. **`EXTRACT(part FROM date)` + `MOD`** (2.10) — cierra **IC10**. Arms en `eval_call` / `eval_binop`.
6. **List comprehension `[x IN list | expr]`** (2.10) — cierra **IC14**. `Expr::ListComprehension` + runtime sobre `Value::List`.

Con (3)–(6) los 14 IC corren. Named paths (1) y `COLLECT_LIST` (2) ya están; el siguiente paso crítico es (3), que IC1 e IC12 comparten.

## Recomendación

Si el objetivo es cerrar los 14 LDBC IC, seguir el *Roadmap para los 14 IC completos* de arriba: named paths (2.9) ya está hecho, así que el siguiente paso es `COLLECT_LIST`, luego `CASE WHEN`, `EXTRACT`/`MOD` y list comprehension.

Si el orden es por valor para queries de usuario en general:

1. **WITH / NEXT** (Tier 2.1). Sin esto no hay pipeline declarativo y todas las queries con agregación filtrada se vuelven imposibles. Mayor lever pendiente, mayor superficie de cambio (AST, typechecker, runtime).
2. **Resto de funciones built-in** (2.5). La infraestructura `Expr::Call` ya existe (`FLOOR`/`CAST`, las path-functions); agregar funciones es incremental.
3. **OFFSET y multi-DML chains** (Tier 2.2 + 2.7). Cierre de huecos sintácticos pequeños que la gente espera.
4. **Multi-DML + WAL** sólo si el caso de uso pasa de research a producción.

Lo que **no** priorizar todavía: transacciones reales (irrelevante para investigación de semántica), MVCC (lo mismo). Shortest path ya está implementado vía los prefijos §16.6 (Tier 2.4); división, `FLOOR`, `CAST`, `RECORD`, `VALUE` y `GROUP BY <var>` cerraron en el cierre de IC7.
