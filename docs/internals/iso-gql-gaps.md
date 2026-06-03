# ISO GQL: qué falta en froGQL

Este documento lista lo que falta en froGQL respecto al estándar ISO/IEC 39075:2024, ordenado por impacto en las queries que los usuarios efectivamente escriben. Sirve como roadmap de features pendientes; lo ya implementado vive bajo "Estado actual".

## Estado actual (cierre de MVP-1)

Implementado y cubierto por tests:

- **Parser y query language**: `MATCH`, `OPTIONAL MATCH`, `WHERE`, `RETURN` (con `DISTINCT` y alias `AS`), `ORDER BY ... ASC|DESC NULLS FIRST|LAST`, `LIMIT`, `GROUP BY`, comma-join.
- **Path patterns**: concat, union (`|`), filter (`WHERE`), repetición `{n,m}`, optional (`?`), aristas dirigidas, reversas y no dirigidas, labels conjuntivas (`A & B`), disyuntivas (`A | B`) y negadas (`!A`).
- **Path-pattern prefixes (ISO §16.6)**: path modes `WALK` / `TRAIL` / `SIMPLE` / `ACYCLIC` y path searches `ALL` / `ANY [N]` / `SHORTEST [N] [PATHS]` / `SHORTEST N GROUPS` (con las formas normalizadas `ANY SHORTEST` y `ALL SHORTEST`). Habilitan repetición ilimitada (`*`, `+`) vía búsqueda k-shortest sobre walks o enumeración finita podada por modo. Aislamiento §16.6 SR 5–8 verificado en el typechecker.
- **Tipos y valores**: `Int`, `Float`, `Str`, `Bool`, `List`, `Record` (anidable), `Null` con lógica trivalente. `Value::Node` y `Value::Edge` como reference values de primera clase.
- **Predicados existenciales**: `EXISTS { ... }` y `NOT EXISTS { ... }` con correlación, fold a literal cuando el body es trivialmente vacío.
- **Aggregation (Feature GF10 parcial)**: `COUNT(*)`, `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`. Null elimination automática y agregados vacíos que producen `null`. Aritmética sobre agregados en la proyección (`COUNT(DISTINCT x) + COUNT(DISTINCT y) AS total`): un agregado puede ser operando de un `Binop`, evaluado por grupo tras el `GROUP BY`.
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

`size(list)`, `length(path)`, `head`, `tail`, `nodes(p)`, `edges(p)`, `type(edge)`, `labels(node)`. Hoy sólo `COALESCE` está soportada como Token dedicado. Sin tabla de funciones general muchas queries quedan awkward. Cambio: agregar `Expr::Call { name, args }` y un dispatch table en `runtime/engine.rs`.

#### 2.6 COLLECT y STDDEV (Feature GF10 completa)

Las agregaciones que faltan respecto a la lista del estándar. `COLLECT_LIST(x)` arma un `Value::List` por grupo; `STDDEV` y `STDDEV_POP` son aritméticas. Encajan en la misma infraestructura `GeneralSetKind` que `SUM`/`AVG`.

#### 2.7 Multi-DML chains en un solo statement

ISO §13.1 permite `MATCH α INSERT β SET γ MATCH δ DELETE ε` como una sola "linear data-modifying statement". El parser actual acepta a lo más un op DML por statement. Los tests de MVP-1.D actualmente parten en dos llamadas a `run_dm` lo que en ISO sería un único statement.

#### 2.8 String escapes en literales

El lexer no admite escapes (`'don\'t'` no parsea, `''` se lee como string vacío). Bloquea `.dump-gql` para nodos cuyas propiedades string contengan `'`. Fix conceptualmente trivial; toca `Lexer::tokenize` y la simétrica en `format_gql_value`.

### Tier 3: producción, no investigación

#### 3.1 Transacciones reales (WAL, recovery)

Hoy `.save` es la única primitiva de "commit"; entre saves la sesión acumula RAM proporcional al overlay. Un crash entre saves pierde toda la mutación posterior al último save. Una WAL real exigiría dirty page tracking, recovery loop al open, y locking inter-conexión. Trabajo de varias semanas, sin demanda en research.

#### 3.2 Concurrencia inter-sesión

`Connection` es `unsendable` en Python y la implementación es single-threaded por design. Habilitar lectores concurrentes pide MVCC sobre el page cache; escritores concurrentes piden además WAL.

#### 3.3 Indexes incrementales bajo overlay

Cuando hay overlay no vacío, `lookup_node_eq` y `lookup_node_range` retornan `None` y el caller hace scan. Mantener hash y btree incrementales durante DML cuesta O(log N) por mutación; cabe en MVP-2.

## Recomendación

Si el orden es por valor para queries reales:

1. **WITH / NEXT** (Tier 2.1). Sin esto no hay pipeline declarativo y todas las queries con agregación filtrada se vuelven imposibles. Mayor lever pendiente, mayor superficie de cambio (AST, typechecker, runtime).
2. **Funciones built-in y UNWIND** (Tier 2.5 + 2.3). Bajo costo, alto retorno: completan el lenguaje sin tocar la semántica del matching.
3. **OFFSET y multi-DML chains** (Tier 2.2 + 2.7). Cierre de huecos sintácticos pequeños que la gente espera.
4. **Multi-DML + WAL** sólo si el caso de uso pasa de research a producción.

Lo que **no** priorizar todavía: transacciones reales (irrelevante para investigación de semántica), MVCC (lo mismo). Shortest path ya está implementado vía los prefijos §16.6 (Tier 2.4).
