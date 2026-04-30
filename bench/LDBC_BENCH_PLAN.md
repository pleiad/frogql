# Plan: usar `substitution_parameters-sf0.1/` para correr LDBC SNB Interactive/BI en gqlrust

## Context

`bench/social_network-sf0.1-CsvBasic-LongDateFormatter/substitution_parameters-sf0.1/`
contiene los **substitution parameters** de LDBC SNB v2.2.4 (PDF §3.4.4 +
Tablas D.8/D.9): bindings concretos producidos por Datagen para llenar
los placeholders de cada query template y simular un workload realista
con cardinalidades calibradas.

- 14 archivos `interactive_<N>_param.txt` (IC1..IC14)
- 25 archivos `bi_<N>_param.txt` (BI1..BI25)

Formato (pipe-delimitado, una row por instancia):

```
personId|firstName        <- header = nombres de los placeholders
30786325579101|Ian
24189255811707|Jun
...
```

Cada IC/BI query del benchmark tiene una plantilla con esos placeholders;
el driver oficial de LDBC sustituye y mide latencia y throughput. Para
gqlrust **falta solo el puente**: hoy `bench_queries`
(`gqlrust/src/bin/bench_queries.rs`) ejecuta queries estáticas, no
soporta sustitución por filas. Tampoco hay traducciones GQL de los
templates IC/BI.

## Qué hay que construir

1. **Runner con `--params <file>`**: lee el header (una línea), parsea
   las filas siguientes y, para cada query del archivo de queries, hace
   sustitución textual de `$placeholder` antes de compilar y correr.
   Reporta `query_id;param_index;result_count;elapsed_ns` (mismo formato
   que CompactLTJ).
2. **Templates GQL para un subconjunto de IC/BI**: escribir en
   `bench/queries/ldbc/<id>.gql` cada plantilla. No tenemos ninguna
   todavía.
3. **Script de orquestación**: shell que itera el set de queries
   seleccionadas, asocia cada una con su `param_*.txt`, invoca el
   runner.

## Limitaciones del lenguaje hoy

`grep`-eando `parser/grammar.rs` y `syntax/query.rs`, gqlrust soporta:

- `MATCH ... WHERE ... RETURN`
- Path repetition `{n,m}`
- Aggregates + `GROUP BY` (per `CLAUDE.md`)
- Joins via comma
- Inline value-filter `{k: v}` (elaborado a WHERE)

Pero **no soporta** `ORDER BY` ni `LIMIT` como cláusulas
(`run_with_limit` impone solo un cap de filas en runtime, sin orden
definido). Esto significa que para queries IC que requieren "top-20 por
distancia, apellido, id" (IC1, IC2, IC5, IC8, etc.), los resultados
salen sin ordenar — útil para medir tiempo y conteo, no para validar
contra el reference output de LDBC.

Tampoco hay `OPTIONAL MATCH`. Las IC que devuelven listas opcionales
(emails, idiomas, organizaciones) habría que descomponerlas en
sub-queries o aproximarlas.

## Recomendación

**MVP de un solo query (IC2) primero, luego iterar.** IC2 es más simple
que IC1: no requiere OPTIONAL MATCH ni la lista variable de campos
opcionales (emails, idiomas, organizaciones). Estructura:
friend-of-friend → mensaje, con un filtro de fecha. Si el runner
funciona en IC2, escalar a IC5/BI4/etc. con la misma estructura es
mecánico.

### Cambios concretos

**1. `gqlrust/src/bin/bench_queries.rs` — añadir `--params <file>`**

- Cuando `--params` está presente, leer header → vector de placeholder
  names.
- Para cada row, construir un `HashMap<&str, String>`.
- Antes de `compile(query_str)`, aplicar `substitute(&template, &row)`
  que reemplaza cada `$<name>` por el valor formateado:
  - Si el valor parsea como `i64` → se inserta literal sin comillas
    (`12345`).
  - Si no → se inserta entrecomillado (`"Jun"`), con escape de `"`
    interior si aparece.
- Output: una línea por (qid, param_index):
  `<qid>;<param_idx>;<count>;<elapsed_ns>`.
- N = 1 disparo por par (query, params row); reportar latencia tal cual.

**2. `bench/queries/ldbc/ic2.gql` — primer template**

Versión MVP de IC2 (sin ORDER BY ni LIMIT 20 — gramática no los
soporta; el driver LDBC los aplica en post-procesamiento, aquí medimos
latencia y conteo bruto):

```
MATCH (p:Person {id: $personId})~[:knows]~(friend:Person)<-[:hasCreator]-(m)
WHERE m.creationDate <= $maxDate
RETURN friend.id, friend.firstName, friend.lastName, m.id, m.creationDate
```

Notas sobre simplificaciones respecto al template oficial:

- LDBC usa `Message` (super-label de Comment + Post). Nuestro schema
  inferido los tiene como labels separados. La query queda permisiva —
  matchea cualquier cosa con hasCreator hacia Person; en LDBC esto
  incluye Comment y Post, suficiente para timing.
- Sin `COALESCE(content, imageFile)` (no hay COALESCE en el lenguaje
  actual). Reportamos `m.id` y `m.creationDate`, omitimos el body.
- Sin ORDER BY DESC / LIMIT 20. El runner usa `--limit N` que es un cap
  de filas sin orden.

**3. `bench/scripts/run_ldbc.sh` — orquestación**

```bash
DB=examples/ldbc-sf01.gdb
PARAMS=bench/social_network-sf0.1-CsvBasic-LongDateFormatter/substitution_parameters-sf0.1
mkdir -p bench/results
./target/release/bench_queries "$DB" bench/queries/ldbc/ic2.gql \
    --params "$PARAMS/interactive_2_param.txt" \
    --limit 100 --timeout 60 \
    > bench/results/ldbc_ic2.csv
```

**4. Tests / smoke**

- Build release.
- Importar el dataset si no existe `examples/ldbc-sf01.gdb`.
- Correr `run_ldbc.sh`; verificar que las 15 filas de
  `interactive_2_param.txt` produzcan 15 líneas de timing en
  `bench/results/ldbc_ic2.csv`.

## Archivos críticos

- `gqlrust/src/bin/bench_queries.rs` — extender con `--params` y
  substitución `$name`. Reusar `LazyGraphStore::open`, `Runtime::new`,
  `gqlrust::compile`, `rt.run_with_limit`.
- `gqlrust/bench/queries/ldbc/` — nuevo directorio, contenedor de
  templates `.gql` con placeholders.
- `gqlrust/bench/scripts/run_ldbc.sh` — orquestación.
- `gqlrust/Cargo.toml` — sin cambios.

## Funciones existentes a reusar

- `LazyGraphStore::open` (`src/store/lazy.rs`) — abrir el .gdb importado
  con `--import-ldbc-csv`.
- `gqlrust::compile` / `compile_query_unchecked` (`src/lib.rs`) —
  pipeline parse+elaborate+optimize.
- `Runtime::run_with_limit` (`src/runtime/engine.rs`) — early
  termination con cap N.

## Verification

```bash
cd gqlrust
cargo build --release --bin bench_queries

# Crear el .gdb si no existe
[ -f examples/ldbc-sf01.gdb ] || ./target/release/gqlite examples/ldbc-sf01.gdb \
    --import-ldbc-csv bench/social_network-sf0.1-CsvBasic-LongDateFormatter/

# Correr IC2
./target/release/bench_queries examples/ldbc-sf01.gdb \
    bench/queries/ldbc/ic2.gql \
    --params bench/social_network-sf0.1-CsvBasic-LongDateFormatter/substitution_parameters-sf0.1/interactive_2_param.txt \
    --limit 100 --timeout 60

# Esperado: ~15 líneas, una por row de params, formato `0;<i>;<rows>;<ns>`
```

## Decisiones confirmadas

1. **Alcance**: solo IC2 como POC. IC1 queda postergada porque su
   template canónico requiere OPTIONAL MATCH (campos opcionales del
   Person resultado), aún no soportado.
2. **Sustitución**: auto-quote por tipo. El runner detecta i64 vs string
   y entrecomilla solo strings.
3. **Repeticiones**: N = 1 disparo por (query, params row). Iterable
   luego si se necesita estabilidad estadística.
