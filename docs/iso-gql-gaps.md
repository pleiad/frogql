# ISO GQL: qué falta en gqlite

Este documento lista lo que falta en gqlite respecto al estándar ISO GQL, ordenado por impacto en las queries que los usuarios efectivamente escriben. Sirve como roadmap de features, no como checklist de conformidad.

## Estado actual

gqlite ya implementa:

- Parser: `MATCH`, `WHERE`, `RETURN` (con `DISTINCT` y alias `AS`), patterns de nodos/aristas con labels, descriptores de tipo.
- Path patterns: concat, union (`|`), filter (`WHERE`), repetición `{n,m}`, optional `?`, join (`,`).
- Valores: `Int`, `Float`, `Str`, `Bool`, `List`, `Record` (anidable).
- Tipos: lattice con `Star`, `Zero`, `Union`, `List`, `Record`, `PropertyType` (open/closed).
- Operadores: `+`, `-`, comparaciones, `=`, `!=`, `AND`, `OR`, `NOT` (sobre booleanos), `IS`, `AS`, `IN`.
- Storage: formato `.gdb` de una sola página-file, embedded.
- Optimizer: predicate pushdown, label index selection, Leapfrog Triejoin.
- `Value::Null`, lecturas de propiedad ausente como null en `AttrLookup`, y lógica trivalente en expresiones generales de `WHERE` (`run_expr` / `eval_binop`). Los predicados empujados al scan/LTJ siguen usando `cmp_values` (null → false), no la misma 3VL que el `WHERE` residual.

## Tier 1: expresividad que rompe cosas si falta

### 1.1 Aggregation: COUNT, SUM, AVG, MIN, MAX, COLLECT

Sin esto no hay reporting. Cualquier query que pregunte "cuántos", "promedio de", "lista de todos los X por Y" es imposible.

```
MATCH (p: Person)-[:ACTED_IN]->(m: Movie)
RETURN p.name, COUNT(m) AS movies, COLLECT(m.title) AS titles
```

Cambios necesarios:

- `Expr::Aggregate { func, arg }` en el AST.
- Pase en `elaborate/` que detecta aggregation en `RETURN` y agrupa por las columnas no-agregadas.
- Runtime con acumuladores por grupo.

### 1.2 Negación a nivel patrón: NOT EXISTS

El `NOT` actual opera sobre valores booleanos (`WHERE NOT x.isBlocked`). Falta la negación estructural sobre patrones:

```
MATCH (a: Person)
WHERE NOT EXISTS {
  (a)-[:ACTED_IN]->(m: Movie)<-[:ACTED_IN]-(k: Person)
  WHERE k.name = 'Keanu Reeves'
}
RETURN a.name
```

Cambios necesarios:

- `PathPattern::NotExists(Box<PathPattern>)` como nueva variante.
- Runtime: evaluar el patrón interno, descartar filas del externo cuando produzca al menos una.
- Semánticamente es anti-join.

### 1.3 OPTIONAL MATCH

Outer join. "Usuarios, y si tienen dirección, la ciudad". La ISO lo llama `OPTIONAL MATCH`, Cypher usa la misma palabra:

```
MATCH (u: User)
OPTIONAL MATCH (u)-[:LIVES_AT]->(a: Address)
RETURN u.name, a.city
```

Sin OPTIONAL, la única forma de expresar "tal vez existe" es `UNION` de dos queries distintas.

Cambios necesarios:

- `PathPattern::Optional(Box<PathPattern>)` o nueva construcción en `Query`.
- Runtime: left-join sobre las variables ya vinculadas, con NULL para las no-matcheadas.
- Depende de 1.4.

### 1.4 NULL y lógica trivalente

**Parcialmente cubierto:** hay `Value::Null`, propiedades ausentes en entidades ligadas leen como null (no `Failure`), y `AND`/`OR`/`NOT`/comparaciones aritméticas siguen 3VL tipo SQL en el evaluador de expresiones del `WHERE` residual.

Siguen fuera (features / alineación ISO futura):

- Predicado `<property exists>` (ISO 19.13): sintaxis y runtime propios; no es el mismo tubo que `x.p`.
- **Follow-up de corrección:** unificar o justificar pushdown (`cmp_values`) vs 3VL del `WHERE` residual cuando el efecto observable del filtro pueda diferir.

OPTIONAL MATCH sigue dependiendo de semántica de variables opcionales además del null en expresiones.

## Tier 2: útiles, hay workaround

### 2.1 ORDER BY / OFFSET / LIMIT como cláusulas

Hoy `limit` es parámetro de `run_query`, no parte de la sintaxis. Agregar `ORDER BY`, `OFFSET`, `LIMIT` al AST de `Query` y ejecutarlos como post-proceso del proyectado.

### 2.2 WITH / pipelines

Encadenar etapas:

```
MATCH (a: Person)-[:ACTED_IN]->(m)
WITH a, COUNT(m) AS c
WHERE c > 10
MATCH (a)-[:FOLLOWS]->(f)
RETURN a.name, f.name
```

En GQL es `NEXT` o `LET`. Permite queries complejas sin subqueries. Interactúa fuerte con aggregation (1.1).

### 2.3 UNWIND

Inverso de COLLECT: dada una lista, emite una fila por elemento.

```
UNWIND [1, 2, 3] AS x
RETURN x * 10
```

Natural ahora que `List` es tipo de primera clase.

### 2.4 Shortest path

`shortestPath()`, `allShortestPaths()`. La repetición `{n,m}` actual hace paths de longitud acotada pero no "el más corto". Algorítmicamente, es BFS dirigido.

### 2.5 Funciones built-in

`size(list)`, `length(path)`, `head`, `tail`, `nodes(p)`, `edges(p)`, `type(edge)`, `labels(node)`. Sin llamadas a funciones, muchas queries quedan awkward. Requiere agregar `Expr::Call { name, args }` y una tabla de funciones registradas.

## Tier 3: relevante para producción, no para investigación

### 3.1 Mutación: INSERT / UPDATE / DELETE

Necesario para un motor real, no para un prototipo de investigación sobre semántica de matching.

### 3.2 Transacciones, sesiones, DDL de grafos

Infraestructura de producción. Irrelevante para el paper.

## Recomendación

Si el paper es sobre semántica de path matching, priorizar en este orden:

1. **Negación** (Tier 1.2). Conceptualmente compacta, cabe en la lattice de tipos, la ISO la define como anti-join. Poca superficie, mucho poder expresivo.
2. **OPTIONAL + NULL** (Tier 1.3 + 1.4). Van juntos: NULL sin OPTIONAL es raro, OPTIONAL sin NULL no tiene semántica limpia. Toca `Value`, `eval_binop` (trivalente), y el pattern runtime (outer-join).
3. **Aggregation** (Tier 1.1). Más invasiva porque cambia el shape del pipeline, pero sin esto las demos se ven mal.

Lo que **no** priorizar ahora: ORDER BY, WITH, UNWIND, funciones. Son quality-of-life pero no tocan semántica profunda.
