# Plan: BFS fast-path para SHORTEST (cierra IC1, IC13, IC14)

> **ESTADO: implementado (2026-06-04).** Niveles 1+2 del plan entregados como
> `Runtime::try_shortest_bfs` en `src/runtime/engine.rs`, enganchado en el arm
> `Selected` antes del fallback `run_repetition_shortest`. Conduce desde el
> lado de menor cardinalidad (sin necesitar la bidireccional explícita del
> nivel 2 para alcanzar milisegundos), reconstruye caminos vía DAG de
> predecesores y reproduce la semántica WALK para extremos coincidentes.
> Verificado contra `bench/data/ldbc-sf0.1.gdb` (backend lazy):
> **IC1 ~75 s→~33 ms**, **IC13 ~27 s→~20 ms** mediana por fila; **IC14**
> deja de hacer OOM (~0.7 s mediana, resultados reales). Suite diferencial
> BFS≡genérico en `tests/shortest_bfs_test.rs`; `GQLITE_DISABLE_SHORTEST_BFS=1`
> lo apaga. Pendiente futuro: nivel 3 (producto NFA×grafo) para uniones de
> label, modos restrictivos y `SHORTEST k>1`. Open items abajo (IC12, IC10)
> siguen vigentes. El placeholder `$`→`{}` de IC1/IC14 se normalizó en sus
> toml para poder correrlos por `ldbc_bench`.

Handoff para retomar en otra sesión. Objetivo: reemplazar la enumeración de
walks por una BFS con dominancia por nodo en el caso común de shortest path
no ponderado, que es el que miden los LDBC IC.

## TL;DR

El runtime resuelve `SHORTEST ... ~*` enumerando **walks** (caminos que
repiten nodos) en un heap por longitud, sin conjunto "visitado" a nivel de
nodo. El número de walks de longitud ≤ d crece como b^d → explota. Hay que
detectar el caso "shortest path no ponderado entre nodos de frontera" y
enrutar a una **BFS** (bidireccional cuando ambos extremos están pinneados),
con dominancia por nodo y sin materializar walks. Colapsa 27–75 s y OOM a
milisegundos.

## Estado verificado (corrido contra `bench/data/ldbc-sf0.1.gdb`, backend lazy, 2026-06-04)

Todo lo de abajo ya está en `main` (named paths, `COLLECT_LIST`, GROUP BY /
ORDER BY por alias, `CASE WHEN`, `MOD`, list comprehension, subqueries
correlacionadas). Compilan parse+typecheck los 14 IC menos IC10. Corriendo de
verdad con params reales:

| IC | Corre | Resultado | Costo | Diagnóstico |
|----|-------|-----------|-------|-------------|
| IC1  | sí | **correcto** (13 cols, 2 `COLLECT_LIST`) | ~75 s/fila, ~5 GB | shortest path `~[:knows]~{1,3}` lento |
| IC12 | sí | **count=0 en las 15 filas** | ~0.1 s/fila | **bug semántico aparte**, no de performance |
| IC13 | sí | **correcto** (int por fila) | ~27 s/fila, ~9 GB | shortest path `~[:knows]~*` lento |
| IC14 | arranca | **OOM ~110 s, 0 filas** | revienta RAM | `ALL SHORTEST ~*` + 2 `VALUE` subqueries por camino |

Conclusión: IC1 e IC13 son **correctos pero lentos**; IC14 es el mismo
problema llevado al OOM; IC12 es un bug distinto (ver *Open items*).

## Causa raíz (con refs de código)

`src/runtime/engine.rs::run_repetition_shortest` (líneas ~1957–2079):

- Toma `grouped = run_path_pattern(inner).to_group()` (todas las laps de 1
  aplicación del patrón interno) y las concatena por `first_node_id` /
  `last_node_id` con un heap (`ShortestEntry`, ~líneas 60–87) ordenado por
  longitud.
- Admite por presupuesto **por par `(first, last)`** (`fn admit`): k caminos
  (`PATHS`) o k longitudes distintas (`GROUPS`).
- **El defecto**: expande `ResultRow` completas (walks), no nodos. No hay
  dominancia por nodo: un nodo se re-expande por cada walk que pasa por él.
  Para `SHORTEST` entre dos extremos fijos sobre un grafo social
  (branching b alto, diámetro chico), el frente crece como b^d → los 9 GB /
  OOM observados.

El dispatch está en `run_path_pattern`, arm `PathPattern::Repeat { ub: None }`
→ `match self.unbounded_policy.get()` → `UnboundedPolicy::Shortest { count,
groups }` → `run_repetition_shortest` (engine.rs ~1207). El policy lo setea el
arm `PathPattern::Selected` desde el prefijo §16.6
(`unbounded_policy_for`).

## El fix

### Idea central
BFS con **dominancia por nodo**: cada nodo se settle una vez; la primera vez
que el frente toca el destino, esa es la distancia mínima. O(V+E) en vez de
b^d. Reconstruir el/los camino(s) con punteros de predecesor; para IC13 ni
hace falta el camino, solo la longitud.

### Tres niveles (implementar incrementalmente)

1. **BFS de una etiqueta, distancia/1-camino** — cubre `SHORTEST 1` /
   `ANY SHORTEST` sobre `~[:L]~*` o `-[:L]->*`. BFS desde el origen con
   `visited: HashSet<Id>` / `dist: HashMap<Id, u32>` y `pred` para
   reconstruir. Para IC13 (solo `PATH_LENGTH`) basta la distancia.

2. **BFS bidireccional** — cuando AMBOS extremos están pinneados (caso
   IC1/IC13/IC14: `(p1:Person {id: X}) ... (p2:Person {id: Y})`). Buscar
   desde origen y destino alternando hasta que las fronteras se toquen:
   2·b^(d/2) en vez de b^d. Detectar "extremos fijos" desde los
   `value_preds`/descriptores ya pusheados (el id es un point-lookup vía
   índice secundario, ver `lookup_node_eq`).

3. **Caso general (producto NFA × grafo)** — para uniones de etiquetas,
   modos `TRAIL`/`ACYCLIC`, `SHORTEST k > 1`. BFS/Dijkstra sobre el producto
   del autómata del patrón y el grafo. Es la versión que usan Neo4j 5.x
   (GQL `SHORTEST k`) y la familia de papers de shortest-path en GQL. **No
   es necesario para los IC**; dejarlo como fase 3.

Para los IC alcanza con (1) + (2).

### Dónde engancharlo
- Nueva función, p.ej. `run_repetition_shortest_bfs(...)`, llamada desde el
  arm `UnboundedPolicy::Shortest` **antes** de caer a
  `run_repetition_shortest` (que queda como fallback general que respeta
  walks/modos). Patrón idéntico al de los otros fast-paths
  (`try_btree_ltj_real`, `try_ltj`): si la precondición no se cumple,
  devolver `None` y caer al camino existente. Cero regresión.

### Precondición de activación (devolver None si no se cumple)
- El patrón interno es **una sola arista** dirigida o no dirigida con una
  etiqueta única (sin uniones de label, sin nodos intermedios con vars).
- `search` es `ShortestPaths`/`ShortestGroups` con `count == 1`, mode `WALK`
  (los modos restrictivos siguen por `run_repetition_unbounded_mode`).
- (Para bidireccional) ambos extremos resuelven a un conjunto chico de ids
  por los predicados pusheados.
- `count > 1` o multi-label → fase 3 → fallback.

### Cuidado con la materialización del camino
- IC13 solo quiere `PATH_LENGTH(path)` → la BFS necesita la longitud; el
  `Value::Path` se materializa con `pred` solo si la query proyecta el
  camino. Ojo: el named-path bindea desde `ResultRow.paths` (ver
  `run_path_pattern` arm `Named`), así que la BFS debe poblar `paths` con la
  secuencia alternada nodo/arista reconstruida, no solo la distancia, cuando
  haya path var. Reconstruir con `pred` + `edge id` por salto.
- Mantener la semántica `lb == 0` (`*` admite el match de longitud 0: una
  fila por nodo, `a == b`).

## Cómo testear y benchear

### Tests de corrección (rápidos, sin dataset)
`tests/path_prefix_test.rs` ya cubre shortest sobre grafos fixture chicos.
Agregar casos que comparen el resultado del fast-path BFS contra el fallback
de walks (`GQLITE_...` flag para forzar uno u otro, estilo
`GQLITE_ORDERBY_FORCE`). Verificar igualdad de filas en grafos con ciclos.

### Bench contra LDBC (requiere el dataset, gitignored)
Dataset presente local: `bench/data/ldbc-sf0.1.gdb` (1.5 GB) y
`bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1/`.

**Caveats para correr los IC bloqueados vía `ldbc_bench`** (el runner solo
corre `status = "implemented"`):
1. Los toml de IC1/IC12/IC14 usan placeholders `$personId`; el runner espera
   `{personId}` (como IC2). Convertir `$x`→`{x}` antes de correr. (IC13 ya
   usa `{}`.) Esto es una inconsistencia real de esos toml.
2. Flip `status = "blocked"` → `"implemented"`.
3. `param_columns` / `param_types` deben matchear el header del param file
   (`personId|firstName` etc.). Ya están seteados correctamente.

Comando (después de los 2 ajustes, reversibles con `git checkout`):
```
cargo build --release --bin ldbc_bench
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic 13 --iters 1 --warmup 0
```
Lee `RSS after open` y el `wall=...ms` por fila. La línea `ROW ... hash=...`
es el oráculo de equivalencia de filas (debe matchear `_lib/row_hash.py` del
cross-system bench). Objetivo del fix: IC1/IC13 de decenas de segundos a
milisegundos, IC14 que complete sin OOM.

## Open items (no son este fix)

- **IC12 da `count=0` en las 15 filas.** Bug semántico, no de performance.
  Sospechas: el `-[:isSubclassOf]->{0,}` (zero-or-more, caso reflexivo) o el
  `ACYCLIC` envolviendo toda la cadena larga. Depurar con un fixture chico
  que reproduzca `tagClass -[:isSubclassOf]->{0,} (:TagClass {name})`.
- **Placeholders `$` vs `{}`** en los toml de IC1/IC12/IC14: normalizar a
  `{}` (el formato que entiende `ldbc_bench::substitute`).
- **IC10** sigue bloqueado por el predicado temporal de cumpleaños
  (mes/día); necesita soporte temporal ISO real, no `EXTRACT`.

## Punteros de código

- `src/runtime/engine.rs`: `run_repetition_shortest` (a reemplazar/wrappear),
  `ShortestEntry`, `run_path_pattern` (dispatch en arm `Repeat`/`Selected`),
  `unbounded_policy_for`, `UnboundedPolicy`, arm `Named` (materialización del
  path var), `path_edge_len`.
- `src/runtime/path_select.rs`: `apply_path_prefix`, `select_shortest_paths`,
  `select_shortest_groups`, `path_satisfies_mode`.
- `src/syntax/path_prefix.rs`: `PathPrefix`, `PathSearch`, `PathMode`,
  `UnboundedSupport`.
- Índices para el point-lookup de extremos: `lookup_node_eq` (trait
  `GraphAccess`), `src/store/secondary_index.rs`.
- Referencia de motores: Neo4j `shortestPath()` = BFS bidireccional; GQL
  `SHORTEST k` (Neo4j 5.9+) = BFS sobre producto NFA×grafo; Kuzu =
  multi-source BFS vectorizada con visitado denso.
