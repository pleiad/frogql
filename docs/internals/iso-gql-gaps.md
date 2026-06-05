# ISO GQL: qué falta en froGQL

Este documento lista lo que falta en froGQL respecto al estándar ISO/IEC 39075:2024, ordenado por impacto en las queries que los usuarios efectivamente escriben. Sirve como roadmap de features pendientes; lo ya implementado vive bajo "Estado actual".

## Estado actual (cierre de MVP-1)

Implementado y cubierto por tests:

- **Parser y query language**: `MATCH`, `OPTIONAL MATCH`, `WHERE`, `RETURN` (con `DISTINCT` y alias `AS`), `ORDER BY ... ASC|DESC NULLS FIRST|LAST`, `LIMIT`, `GROUP BY`, comma-join.
- **Expresiones de valor (cierre IC7 + expresiones ISO)**: división `/` (ISO `<solidus>`), `FLOOR(<numeric>)`, `CAST(<op> AS INTEGER|FLOAT)` (conversión de valor, distinta de la aserción de tipo `AS`), constructor de record `RECORD { k: <expr>, ... }` con valores-expresión (`RECORD` opcional, fast-path constante), subconsulta de valor `VALUE { MATCH ... RETURN <1 item> ORDER BY ... LIMIT 1 }`, `CASE WHEN ... THEN ... ELSE ... END`, y `MOD(a,b)` funcional. `FLOOR`/`CAST`/`RECORD`/`VALUE` son soft keywords (solo antes de `(`/`{`). `GROUP BY <binding variable>` agrupa por identidad de nodo/arista con chequeo de dependencia funcional ISO §14; `ORDER BY <alias>.<campo>` y `ORDER BY CAST(<alias> AS tipo)` resuelven columnas proyectadas (`SortKey::ColumnField` / `SortKey::ColumnCast`).
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

Cubierto por los path-pattern prefixes: `SHORTEST [N] [PATHS]`, `SHORTEST N GROUPS`, `ANY SHORTEST`, `ALL SHORTEST`, más los modos `TRAIL` / `SIMPLE` / `ACYCLIC` que hacen finita la repetición ilimitada. El runtime usa una búsqueda k-shortest sobre walks (`run_repetition_shortest`, heap ordenado por longitud con presupuesto por par `(origen,destino)`) o enumeración podada por modo (`run_repetition_unbounded_mode`); no decompone a triples LTJ. Ver `src/runtime/path_select.rs` y la sección *Path-pattern prefixes* del CLAUDE.md de la crate.

**Fast-path BFS (2026-06-04).** La búsqueda k-shortest sobre walks enumera caminos en un heap sin dominancia por nodo, así que el frente crece como `b^d` y revienta la RAM en grafos sociales. El arm `Selected` intenta primero una BFS con dominancia por nodo (`try_shortest_bfs`, O(V+E) por origen) para la forma canónica `(src) [arista-única]{lb,ub} (tgt)` bajo WALK + `SHORTEST 1 PATHS|GROUPS`, con `lb ≤ 1` y arista sin variable; cualquier otra forma cae al camino genérico (cero regresión, garantizado por el suite diferencial `tests/shortest_bfs_test.rs`). Maneja extremos sin pinear, conduce desde el lado más chico (recorriendo la adyacencia en reversa cuando conduce desde el destino), reconstruye caminos vía DAG de predecesores (todos los de longitud mínima para `GROUPS`, uno para `PATHS`) y reproduce la semántica WALK para extremos coincidentes (camino de longitud 0 bajo `*`; walk cerrado trivial `n-e-m-e-n` bajo `+`/`{1,…}`, acotado por `ub`). Colapsa **IC1 ~75 s→~33 ms** y **IC13 ~27 s→~20 ms** por fila (SF0.1, backend lazy), y desbloquea el **OOM de IC14** (ahora ~0.7 s mediana con resultados reales; el costo restante son las dos subqueries `VALUE` por camino). `GQLITE_DISABLE_SHORTEST_BFS=1` lo apaga.

#### 2.5 Funciones built-in

Existe `Expr::Call { name, args }` con dispatch en `engine.rs::eval_call`; hoy resuelve `FLOOR` y `CAST` (más `COALESCE`/`DURATION` por Token dedicado), `MOD(a,b)` como expresión numérica (`BinOp::Mod`), y las path-functions `ELEMENTS` / `PATH_LENGTH` / `CARDINALITY` / `NODES` / `EDGES` (ver 2.9). Faltan `size(list)`, `head`, `tail`, `type(edge)` y `labels(node)`. Para IC10 también falta soporte temporal ISO real para expresar la ventana de cumpleaños; no usar `EXTRACT(MONTH|DAY FROM ...)` como atajo porque no es sintaxis GQL ISO en ISO/IEC 39075:2024.

#### 2.6 COLLECT y STDDEV (Feature GF10 completa)

`COLLECT_LIST(x)` ya está implementado. Faltan `STDDEV` y `STDDEV_POP`, que son aritméticas y encajan en la misma infraestructura `GeneralSetKind` que `SUM`/`AVG`.

#### 2.7 Multi-DML chains en un solo statement

ISO §13.1 permite `MATCH α INSERT β SET γ MATCH δ DELETE ε` como una sola "linear data-modifying statement". El parser actual acepta a lo más un op DML por statement. Los tests de MVP-1.D actualmente parten en dos llamadas a `run_dm` lo que en ISO sería un único statement.

#### 2.8 String escapes en literales

El lexer no admite escapes (`'don't'` no parsea, `''` se lee como string vacío). Bloquea `.dump-gql` para nodos cuyas propiedades string contengan `'`. Fix conceptualmente trivial; toca `Lexer::tokenize` y la simétrica en `format_gql_value`.

#### 2.9 Named path patterns y path functions — implementado (2026-06-03)

`MATCH path = (a)-[:k]->(b)` liga el patrón completo a una variable-camino. La declaración ISO `<path variable declaration> ::= <binding variable> =` se parsea por operando de comma-join, fuera del prefijo §16.6 (`Named { var, Selected { prefix, pattern } }`), y materializa un `Value::Path` proyectable. Las path-functions de §20.16 operan sobre él:

- `ELEMENTS(path)` — lista de todos los elementos (nodos y aristas) en orden de match (conforme ISO).
- `PATH_LENGTH(path)` — número de aristas (conforme ISO).
- `CARDINALITY(path)` — número total de elementos, nodos más aristas (conforme ISO).
- `NODES(path)` / `EDGES(path)` — proyecciones nodo-solo / arista-solo. **No** son ISO (§20.16 no las define); se ofrecen como **divergencia de traducción** para los queries LDBC que las usan. Anótalas como divergencia en los toml afectados.

Implementación: variantes `PathPattern::Named`, `PathValue::Path`, `Value::Path`, `SimpleType::Path`, `VariableType::Path` (terminal en el retículo: solo se encuentra consigo misma, nunca refina contra schema, nunca vacía un entorno). El typechecker liga la variable-camino en el `TypeEnvironment` y exige que el argumento de cada path-function tipe como `Path` (un argumento demostrablemente no-camino es error de tipo). El runtime captura la secuencia ya construida en `ResultRow.paths` (no recomputa). Tests en `tests/named_path_test.rs`.

#### 2.10 CASE WHEN, MOD, list comprehension, temporales

Estado de expresiones que bloqueaban ICs:

- `CASE WHEN <cond> THEN <e> ELSE <e> END` (`<case expression>`) — implementado.
- `MOD(a,b)` (`<modulus expression>`) — implementado con la forma funcional ISO; no se acepta `a MOD b`.
- IC10 sigue necesitando expresar una ventana de cumpleaños por mes/día. La forma `EXTRACT(MONTH|DAY FROM ...)` no es ISO GQL en ISO/IEC 39075:2024, por lo que no se implementa como superficie del lenguaje. Falta soporte temporal ISO real o una traducción ISO-conformante equivalente.
- List comprehension `[x IN <source> [WHERE <f>] | <body>]` (`<list value constructor by enumeration>` con filtro/map) — **implementado (2026-06-04)**. Nuevo `Expr::ListComprehension { var, source, filter, body }`. El parser la desambigua de un literal de lista por el separador `|` (con rewind). `var` se liga localmente a cada elemento del `source` (un `Value::List`): el typechecker la tipa como `List(Star)` vía un *scope stack* (`comprehension_scope` en `Typechecker`), y el runtime usa un scope análogo (`comprehension_scope` en `Runtime`, `RefCell<Vec<(String, Value)>>`) porque `Assignment` solo guarda `PathValue` y no puede contener elementos escalares; `Var`/`AttrLookup` consultan ese scope antes que la fila de binding. Source nula/vacía → lista vacía; un elemento que falla mapea a `Null` (3VL). Tests en `tests/list_comprehension_test.rs`.

#### 2.11 COLLECT_LIST / multiset — implementado (2026-06-03)

`COLLECT_LIST(x)` (alias `COLLECT` / `ARRAY_AGG`) arma un `Value::List` por grupo. Es un `GeneralSetKind::CollectList` cuyo reducer en `apply_aggregator` envuelve los valores ya recolectados (con eliminación de nulls y `DISTINCT` heredados de `collect_aggregate_values`) en `Value::List`. Tipa como `List(elem)`. Además dropea records all-null, que vienen del lado vacío de un `OPTIONAL MATCH` (`RECORD { a: opt.x }` con `opt` sin match → todos los campos null) y representan "sin fila". Tests en `tests/collect_list_test.rs`.

Era prerequisito de IC1 e IC12; ambos además agrupaban por *alias* de RETURN, ya resuelto (ver 2.12).

#### 2.12 GROUP BY por alias de RETURN — implementado (2026-06-04)

`GROUP BY <binding variable>` agrupa por identidad de nodo/arista (única forma estricta ISO: `<grouping element> ::= <binding variable reference>`). froGQL ya admitía además agrupar por una expresión deletreada (`GROUP BY n.city`) como desviación documentada. Ahora cierra la conveniencia estilo Cypher/SQL `GROUP BY <alias>` donde `<alias>` es un nombre de columna de RETURN (`friend.id AS friendId ... GROUP BY friendId`).

Implementación: una pre-pasada de **resolución de nombres en elaboración** (`src/elaborate/mod.rs::resolve_group_key`). Para cada clave de GROUP BY que es un `Expr::Var(name)` bare: si `name` es variable de binding del patrón se deja (gana la variable — shadowing); si es alias de un `RETURN <expr> AS name` se sustituye por la expresión aliaseada (ya elaborada); si no es ninguno se deja para que el typechecker reporte "Variable not found". Va en elaboración (no en el optimizer) porque la resolución la necesitan tanto el typechecker como el runtime de agrupamiento. Tras la sustitución, el chequeo de dependencia funcional pasa por igualdad estructural y el runtime evalúa la expresión real contra las filas. Nota de eficiencia: la clave se evalúa una vez por fila y la proyección una vez por grupo (igual que la forma deletreada); el "compute-once" real requeriría `LET` (2.1). Tests en `tests/group_by_alias_test.rs`.

Pendiente menor: alias **dentro** de una expresión en GROUP BY (`GROUP BY CAST(friendId AS INTEGER)`) — solo se resuelve el alias bare. Para ORDER BY, `<alias>` a secas lo resuelve `order_by_alias.rs`, y `<alias>.<campo>` / `CAST(<alias> AS tipo)` ya están vía `SortKey::ColumnField` / `SortKey::ColumnCast`.

#### 2.13 WHERE correlacionado en cuerpos de subquery — implementado (2026-06-04)

Un cuerpo de `EXISTS { ... }` / `VALUE { ... }` puede referenciar en su WHERE/RETURN una variable **externa** que su propio patrón no liga (correlación por *parámetro*, distinta de compartir una variable de patrón). Antes fallaba con "Variable X not found in context" y, saltando el typecheck, el runtime devolvía vacío/Null. Lo dispara IC14: `WHERE pe1 IN NODES(path) AND pe2 IN NODES(path)` dentro de dos `VALUE { ... }`, donde `path` es la variable de camino externa.

**Causa raíz**: `freevars` ignora las variables del WHERE (`Filter`), así que la detección de correlación trataba el cuerpo como no-correlacionado; y el typechecker chequeaba el `Filter` del cuerpo contra el entorno local del patrón, sin las variables externas.

**Fix (typecheck)**: un stack `ambient_env` en el `Typechecker` lleva el entorno externo mientras se chequea el cuerpo; el arm `Filter` lo fusiona bajo las ligaduras locales del patrón. Scoped solo a cuerpos de subquery — un WHERE cross-cláusula a nivel top-level conserva su scope estricto (sigue rechazado, ya que el runtime tampoco lo evalúa).

**Fix (runtime)**: `param_correlation` calcula las variables externas *referenciadas* (vía `query_referenced_expr_vars`, que recorre Filter/RETURN/GROUP BY/ORDER BY) menos las que el patrón del cuerpo declara. Cuando existen (y no hay correlación por variable de patrón compartida que el cache semi-join ya cubre), el cuerpo se evalúa **por fila externa** con esos parámetros ligados en un stack ambient `correlation_scope`; `Var`/`AttrLookup` caen a él. La proyección de `VALUE` despacha a agregación cuando el item de RETURN lleva agregado. El fold existencial solo pliega un cuerpo a literal cuando su typecheck standalone no tiene errores (un error de variable no ligada indica correlación → no plegar, dejarlo al runtime).

Tests en `tests/correlated_subquery_test.rs`. Limitación restante: una correlación *mixta* (variable de patrón compartida **y** parámetro a la vez) usa el camino antiguo (cachea por la compartida, ignora el parámetro); ningún IC la usa. El WHERE cross-cláusula a nivel top-level (`MATCH (a) MATCH (b) WHERE a=b`) sigue sin soportarse en ambas capas (requiere hoisting del WHERE post-join, trabajo aparte).

Con esto IC14 corre. La traducción del toml añade un `GROUP BY path` explícito (divergencia documentada): el query pesa cada camino más corto por separado, y el `SUM` agrega los scores por par de personas dentro del camino; el Cypher agrupa por camino implícitamente, GQL ISO exige el elemento de agrupación explícito.

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
| IC1–IC9, IC11, IC12, IC13, IC14 | implementado | — |
| IC10 | blocked | temporal ISO para predicado de cumpleaños por mes/día (2.10). `CASE`, `MOD(a,b)` y `ORDER BY CAST(alias)` ✅ |

Nota de rendimiento (2026-06-05): IC4 e IC7 corrían pero materializaban el cuerpo de su subconsulta correlacionada (`NOT EXISTS` en IC4, `VALUE` arg-max en IC7) sobre todo el grafo, con un piso fijo de ~3 s por parámetro que excedía el tope de 600 s del bench cross-system en hosts lentos. Ahora el cuerpo se evalúa por fila externa con las variables de correlación fijadas a los ids de esa fila (probe pinned, memoizado por tupla): IC4 deja de hacer timeout y IC7 baja de 1305 ms a ~9 ms, con filas byte-idénticas a la referencia. Detalle en `implemented-optimizations.md` §13. El mismo cambio corrigió un bug de corrección en `NOT EXISTS` sobre aristas no dirigidas (`~[...]~`) que el pin introdujo antes de incorporar el gate `has_undirected_edge`.

### Roadmap para los 14 IC completos

Ordenado por leverage (ICs desbloqueados por feature):

1. ~~**Named path patterns + path functions** (2.9)~~ — **hecho (2026-06-03)**. `MATCH path = [ANY|ALL] SHORTEST (...)`, `ELEMENTS`, `PATH_LENGTH`, `CARDINALITY`, más `NODES`/`EDGES` (divergencia). Desbloqueó el prerequisito de **IC1, IC13, IC14**.
2. ~~**`COLLECT_LIST` / multiset aggregate** (2.11)~~ — **hecho (2026-06-03)**. `GeneralSetKind::CollectList`, reducer a `Value::List`, drop de records all-null. Era prerequisito de **IC1 e IC12**, pero ninguno cierra sin (3).
3. ~~**GROUP BY por alias** (2.12)~~ — **hecho (2026-06-04)**. Resolución de nombres en una pre-pasada de elaboración (`resolve_group_key`). Cerró **IC1 e IC12** (este último con la divergencia de traducción `{0,}`→prefijo `ACYCLIC` ya aplicada en su toml, sin código nuevo).
4. ~~**List comprehension `[x IN list | expr]`** (2.10)~~ — **hecho (2026-06-04)**. `Expr::ListComprehension` + scope local en typechecker y runtime. Era prerequisito de **IC14**, pero **no suficiente** (ver 2.13).
5. ~~**WHERE correlacionado en cuerpos de subquery** (2.13)~~ — **hecho (2026-06-04)**. `ambient_env` (typecheck) + `correlation_scope` y `param_correlation` (runtime). Cerró **IC14** (con `GROUP BY path` explícito en su toml, divergencia documentada).
6. **Temporales ISO para la ventana de cumpleaños de IC10** (2.10) — cierra **IC10**. No usar `EXTRACT(...)` como atajo: no es sintaxis ISO GQL.

Queda solo **IC10** (temporales ISO). Named paths (1), `COLLECT_LIST` (2), GROUP BY por alias (3), list comprehension (4), WHERE correlacionado en subqueries (5), `CASE`, `MOD(a,b)` y `ORDER BY CAST(alias)` ya están — **13 de 14 IC corren**.

## Recomendación

Si el objetivo es cerrar los 14 LDBC IC, queda **solo IC10** (soporte temporal ISO real para la ventana de cumpleaños por mes/día; no usar `EXTRACT(...)`, que no es sintaxis ISO GQL). Los otros 13 corren.

Si el orden es por valor para queries de usuario en general:

1. **WITH / NEXT** (Tier 2.1). Sin esto no hay pipeline declarativo y todas las queries con agregación filtrada se vuelven imposibles. Mayor lever pendiente, mayor superficie de cambio (AST, typechecker, runtime).
2. **Resto de funciones built-in** (2.5). La infraestructura `Expr::Call` ya existe (`FLOOR`/`CAST`, las path-functions); agregar funciones es incremental.
3. **OFFSET y multi-DML chains** (Tier 2.2 + 2.7). Cierre de huecos sintácticos pequeños que la gente espera.
4. **Multi-DML + WAL** sólo si el caso de uso pasa de research a producción.

Lo que **no** priorizar todavía: transacciones reales (irrelevante para investigación de semántica), MVCC (lo mismo). Shortest path ya está implementado vía los prefijos §16.6 (Tier 2.4); división, `FLOOR`, `CAST`, `RECORD`, `VALUE`, `GROUP BY <var>`, named paths, path functions, `COLLECT_LIST`, `CASE` y `MOD` ya están implementados.
