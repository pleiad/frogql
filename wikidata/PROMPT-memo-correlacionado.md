# Prompt: `memo` correlacionado por ancla

> **Hecho.** El arm está implementado y verificado; el detalle vive en
> `docs/internals/vector-search.md` (*Correlated `NEAREST`*) y en
> `CLAUDE.md`. Los dos cortes nuevos llevan interruptor de apagado
> (`FROGQL_DISABLE_MEMO_CUTS=1`) y su test diferencial. Falta la medición
> sobre la base real: `wikidata/imgpedia.gdb` no está en esta máquina, así
> que el barrido por nivel se midió sobre un fixture sintético. Sigue
> pendiente el timeout de query de la última sección.

Copia todo lo de abajo en una sesión nueva.

---

Trabaja en `~/Documents/U/GQL/interpreter/gqlrust`, rama `wikidata`.
Lee `CLAUDE.md` y `docs/internals/vector-search.md` antes de tocar nada.

## Qué quiero

Implementar la forma **correlacionada** de la estrategia `memo`, con
alcance **por ancla**, y agregarle dos cortes tempranos que hoy no tiene.

Una cláusula `NEAREST` correlacionada es aquella cuyo vector de consulta
nombra una variable del patrón:

```
NEAREST 50 v21.hog TO VECTOR(v10, 'hog') AS dist
```

`v10` es el **ancla**. Hay un ranking por cada valor distinto de `v10`,
no uno para toda la query.

## Estado actual (ya implementado, no lo rehagas)

- **`post` correlacionado** — `src/runtime/vsearch/correlated.rs::run`.
  Corre el patrón una vez, particiona las filas por ancla, rankea cada
  partición. Funciona.
- **`interleave` correlacionado** — `correlated.rs::try_interleave` →
  `in_ltj::run`. Funciona. El ancla se fuerza **arriba** de la variable
  de búsqueda en el VEO con `VeoOverride::pin_at_after`
  (`src/runtime/ltj/veo.rs`), así que al llegar al nivel de búsqueda el
  ancla ya está ligada y su vector es legible.
- **`VecCtx`** (`src/runtime/ltj/algorithm.rs`) ya tiene `anchor_var`,
  `cur_anchor`, `q_owned`, `use_hnsw` y `retarget()`, que reapunta
  vector + umbral + stream cuando cambia el ancla.
- **`DistThreshold::reset()`** (`src/runtime/vsearch/topk.rs`) — el
  top-k se cuenta **por ancla**.
- **`memo` no correlacionado** — `algorithm.rs::run_nearest_memo`, dos
  fases con `PrefixTable`. Hoy `memo` sobre una cláusula correlacionada
  cae a `post` con `fallback_reason`.
- **`pre` correlacionado** — deliberadamente fuera de alcance. Déjalo
  cayendo a `post` como está.

## El problema medido

Base real: 160.288.041 nodos, 617.065.092 arcos, corpus vectorial de
**1.455.385** vectores de dim 288. Query del benchmark, `k = 50`,
`FROGQL_VEC_LEVEL=4`:

| arm | tiempo | `nn_pops` | `ltj_visits` | `candidates` | `anchor_groups` |
|---|---|---|---|---|---|
| `post+hnsw` | 272,3 s | 31.102.633 | 0 | 197.892 | 478 |
| `interleave+localsort` | **66,9 s** | 173.520 | 225.589 | 227.457 | 414 |
| `interleave+hnsw` | **no termina** | — | — | — | — |

Por qué `interleave+hnsw` no termina: en el nivel 4 cada visita tiene
**~1 candidato**, y el bucle de `interleave` escanea el stream del
corpus **desde la posición 0 en cada visita** buscando ese 1 candidato
entre 1.455.385.

```
225.589 visitas × hasta 1.455.385 posiciones ≈ 3 × 10¹¹ comparaciones
```

El grafo HNSW se camina una vez por ancla (`NnStream` cachea en
`prefix`), pero el **escaneo del cache** se repite por visita.

## Qué hay que construir

`memo` mueve ese escaneo de *por visita* a *una sola vez*. Correlacionado,
"una sola vez" significa **una vez por ancla**:

```
para cada ancla a (las visitas de un ancla son contiguas, el join baja en DFS):

    # fase 1 — juntar, sin bajar
    tabla = {}                        # candidato -> todos los prefijos que llegan a el
    en cada visita al nivel de busqueda bajo `a`:
        para cada candidato c de collect_candidates(var):
            si pasa check_filters: tabla[c].push(prefijo)

    # fase 2 — UN recorrido del indice
    n = tabla.len()
    vistos = 0
    para (id, dist) en indice_desde(vector(a)):
        si dist > tau * (1 + tau_eps): cortar        # ya existe
        si id no esta en tabla: continuar
        vistos += 1
        para cada prefijo de tabla[id]:
            reanudar el join debajo del nivel        # con backtracking
        si sink lleno con k: cortar                  # CORTE NUEVO 1
        si vistos == n: cortar                       # CORTE NUEVO 2
```

### Los dos cortes nuevos

Hoy el bucle de `interleave` solo corta por `tau`, y `tau` vale
**infinito hasta juntar k**. Faltan:

1. **`k` completos** — si el sink ya tiene los `k` de esta ancla y el
   stream pasó el umbral, no hay nada más que encontrar.
2. **todos los candidatos vistos** — si el ancla tiene `n` candidatos y
   ya los viste a los `n` en el stream, seguir es puro desperdicio,
   cualquiera sea `tau`. **Este es el que más importa**: acota el
   recorrido por `n` en vez de por el corpus.

`post_filter::walk_global` ya hace el (2) con un contador `remaining`;
copia esa disciplina, no inventes otra.

## Invariantes que no se pueden romper

1. **`memo` correlacionado debe dar exactamente lo mismo que `post`
   correlacionado y que `interleave` correlacionado**, bajo las fuentes
   exactas (`localsort`, `globalsort`). Si no, los tres responden
   preguntas distintas y comparar sus tiempos no significa nada.
2. **`k` se cuenta por ancla**, no por query. `in_ltj::select_per_anchor`
   ya hace la selección final así.
3. **El umbral se resetea por ancla** (`DistThreshold::reset`). Es
   correcto porque el join baja en profundidad y las visitas de un ancla
   son contiguas; si cambias eso, justifica por qué sigue siendo cierto.
4. **`stats.arm` debe decir lo que corrió de verdad**, no lo que se
   pidió. Si el shape no permite el arm, `fallback_reason` explica por
   qué. Un benchmark que reporta el arm pedido en vez del ejecutado
   miente.
5. **Un `WHERE` residual hace que los arms in-LTJ declinen**
   (`PathPattern::has_residual_filter`). No lo toques: filtrar la salida
   no repara el problema, porque el umbral ya podó con filas que el
   predicado rechaza.

## Archivos

| archivo | qué tiene |
|---|---|
| `src/runtime/vsearch/correlated.rs` | `run_correlated` (despacho), `try_interleave`, `run` (particionar) |
| `src/runtime/vsearch/in_ltj.rs` | arma el `VecCtx`, llama a `try_ltj_nearest`, `select_per_anchor` |
| `src/runtime/ltj/algorithm.rs` | `VecCtx`, `retarget`, el gancho `nn_level` dentro de `search`, `run_nearest`, `run_nearest_memo`, `PrefixTable` |
| `src/runtime/ltj/veo.rs` | `VeoOverride::pin_at`, `pin_at_after` |
| `src/runtime/vsearch/topk.rs` | `TopK`, `DistThreshold` (con `reset`) |
| `src/vector/cursor.rs` | `NnStream` (cachea en `prefix`), `NnCursor` |

## Verificación

Antes de cualquier commit, el checklist de `CLAUDE.md`:

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test                      # la suite completa, no un subconjunto
```

Tests que tienen que seguir pasando y a los que hay que agregar casos:

- `tests/correlated_nearest_test.rs` — el arm correlacionado. Agrega:
  `memo` correlacionado ≡ `post` correlacionado ≡ `interleave`
  correlacionado bajo las fuentes exactas; que `stats.arm` diga
  `memo+<source>` y no un fallback; que los dos cortes nuevos se
  disparen (cuenta `nn_pops`, no el reloj).
- `tests/vector_strategy_equiv_test.rs` — los 11 arms no correlacionados
  siguen coincidiendo.

Y una medición sobre la base real, con `vec_sweep`, que abre una sola vez:

```bash
./target/release/ltj_build wikidata/imgpedia.gdb --compact   # una vez, si no existe
FROGQL_LTJ_COMPACT=1 FROGQL_AUTO_INDEX_KINDS=hash \
./target/release/vec_sweep wikidata/imgpedia.gdb wikidata/una.gql \
  --arms post+localsort,interleave+localsort,memo+localsort,memo+hnsw \
  --levels 2,3,4 --iters 3 --csv memo.csv
```

## Advertencia sobre expectativas

Estimando con los números de arriba: ~545 candidatos por ancla de un
corpus de 1.455.385 es 0,037% de selectividad, así que hallar 50 pide del
orden de 135.000 pops por ancla, × 414 anclas ≈ **56 millones** — del
mismo orden que los 31 millones que gastó `post+hnsw`. O sea: **es
probable que `memo` correlacionado quede parecido a post-filter en el
nivel 4, no mejor.**

Eso también es un resultado válido — a nivel profundo los dos convergen
porque el join ya hizo el trabajo. Donde `memo` debería ganar de verdad
es en **niveles intermedios**: pocos candidatos por visita, muchos por
ancla. Mide 2, 3 y 4, no solo 4. Y si sale peor que `interleave`, dilo;
no lo maquilles.

## Un pendiente aparte, si sobra tiempo

No hay **timeout de query** en ningún lado. `bench_queries` dice tenerlo
pero solo etiqueta la fila *después* de que la query terminó
(`src/bin/bench_queries.rs:91`), no la corta. Con 7 arms × 5 niveles ×
100 queries, una combinación mala deja la corrida colgada días. Haría
falta un presupuesto que el motor consulte en el bucle de `search` y en
el barrido de vecinos, más `--timeout` en `vec_sweep` que marque la fila
y siga. Es independiente de lo de arriba; pregúntame antes de meterte.
