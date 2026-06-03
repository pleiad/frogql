# Repetition range: legacy vs incremental

## Qué cambia

Dos implementaciones del operador `{lb,ub}` conviven, seleccionables por env var:

- **legacy** (default): `engine.rs` itera `for i in lb..=ub` y llama a
  `run_repetition_pattern(p, i)` por cada longitud. Cada llamada evalúa el
  patrón interno desde cero, construye el hash `first → indices`, y desde
  longitud 1 hace `i-1` extensiones para llegar al nivel `i`.
- **incremental** (`GQLITE_REPEAT_INCREMENTAL=1`): evalúa el patrón
  interno una vez, construye el hash una vez, y mantiene un único buffer
  `rows: Vec<ResultRow>` con todos los niveles concatenados. Cada nivel
  `k` se construye iterando el rango de índices del nivel `k-1`. Niveles
  por debajo de `lb` se descartan al final con un `Vec::drain` único.

Sin clones por nivel intermedio. El único clone es la copia de la tabla
del patrón interno al nivel 1 del buffer.

## Setup del bench

`src/bin/bench_repeat.rs` genera un grafo sintético determinista (LCG
con semilla `0xC0FFEE`, aristas dirigidas con `s,t` uniformes) y ejecuta
`(a)-[x]->{lb,ub}(b)` bajo ambos modos en el mismo proceso. Por
configuración: 1 warmup + N iteraciones medidas, mediana reportada.
Fila count se valida idéntico (`parity=OK`) en cada corrida.

Hardware: ejecuciones en la máquina del usuario (Darwin arm64), `cargo
build --release`.

## Resultados

### Configuración pedida: `(a)-[x]->{1,5}(b)` en grafo denso

```
Graph: nodes=30 directed-edges=180 avg-out-deg≈6
Iters: 5

  legacy        rows=320660  median=291669 us
  incremental   rows=320660  median=270887 us
  speedup=1.08x
```

Sweep sobre `ub` con `lb=1`:

| ub | legacy (us) | incremental (us) | speedup | rows    |
|----|-------------|------------------|---------|---------|
| 1  | 176         | 192              | 0.92×   | 180     |
| 2  | 1142        | 924              | 1.24×   | 1298    |
| 3  | 6929        | 5855             | 1.18×   | 8272    |
| 4  | 47231       | 42415            | 1.11×   | 51596   |
| 5  | 292044      | 269605           | 1.08×   | 320660  |

A medida que `ub` crece, el nivel más profundo domina el costo total y
la ventaja relativa del incremental se reduce: el ahorro está en evitar
re-construir niveles bajos, pero esos niveles bajos pesan poco frente
al deepest level.

### Otras formas

| Forma                 | Grafo          | legacy (us) | incremental (us) | speedup |
|-----------------------|----------------|-------------|------------------|---------|
| `{0,3}` denso         | N=30, deg=6    | 7077        | 5968             | 1.19×   |
| `{1,5}` denso         | N=30, deg=6    | 291669      | 270887           | 1.08×   |
| `{1,4}` muy denso     | N=25, deg=12   | 510753      | 458010           | 1.12×   |
| `{1,5}` esparso       | N=100, deg=2   | 6886        | 4825             | 1.43×   |
| `{1,7}` esparso       | N=100, deg=2   | 32201       | 23483            | 1.37×   |
| `{2,2}` denso         | N=30, deg=6    | 931         | 874              | 1.07×   |
| `{3,5}` denso         | N=30, deg=6    | 284201      | 278287           | 1.02×   |

Patrones observables:

1. **Speedup mayor en grafos esparsos** (1.3–1.4×). Cuando los niveles
   crecen lentamente (degree bajo), todos los niveles dentro del rango
   pesan parecido y evitar reconstruirlos `(ub-lb+1)` veces es una
   ganancia tangible.
2. **Speedup menor en grafos densos** (1.05–1.15×). El nivel más
   profundo domina y la legacy ya lo construye una sola vez por su `i`
   máximo; los niveles bajos que la legacy reconstruye son baratos en
   absoluto.
3. **`{lb,ub}` con `lb` grande pierde la ventaja** (1.02–1.08×). Tanto
   legacy como incremental tienen que construir todos los niveles
   1..=ub (la legacy porque cada `i` parte de cero, la incremental
   porque arma el slice y luego dropea el prefijo); la diferencia
   queda en niveles 1..lb-1 reconstruidos `(ub-lb+1)` veces vs. una
   sola.
4. **`{ub,ub}` (rango puntual)** muestra speedup pequeño (~1.07×) por
   el ahorro de una evaluación del patrón interno y la construcción
   única del hash.
5. **`{1,1}` es ligeramente más lenta en incremental** (~0.9×) por el
   overhead del `level_ranges` Vec y el draining trivial; es un caso
   degenerado que no justifica una rama especial.

### Cobertura de tests

Bajo `GQLITE_REPEAT_INCREMENTAL=1`:

- `runtime_test`: 46/46 pass
- `parser_test`, `parse_and_run_test`, `count_test`, `record_test`,
  `list_test`, `elaborate_test`, `optional_match_test`,
  `multi_match_test`, `store_runtime_test`: pass
- `cargo clippy --workspace --all-targets -- -D clippy::all`: clean

`runtime_test.rs:615-680` cubre las semánticas de repetición:
`{1,2}`, `{2,2}`, anidamiento `(-[x]->{1,2}){1,2}`, y `{0,1}`.
Ambos modos producen idéntica row count (`parity=OK`) en cada celda
del bench.

## Costo en código

- `engine.rs`: +103 líneas (`run_repetition_range` + `repeat_incremental_enabled`).
- `engine.rs`: +6 líneas en el match arm de `Repeat` para el dispatch.
- `bench_repeat.rs`: +175 líneas (binario nuevo, no afecta producción).

`run_repetition_pattern` (legacy) sigue viva: la usa el incremental
para el caso `lb=0` (baseline de todos los nodos con bindings vacíos),
así que aunque saquemos la legacy del dispatch, la función no se
elimina del todo.

## Recomendación

**Dejar incremental como default y eliminar el dispatch por env var.**

Razones, en orden de peso:

1. **Speedup positivo en todos los casos no triviales.** El peor caso
   medido en grafos densos es 1.02× (rango `{3,5}`); en sparse y rangos
   anclados en `lb=1` la ventaja sube a 1.4×. El único caso negativo es
   `{1,1}`, donde perdemos ~10% sobre una query que ya corre en 200µs
   absolutos. No es material.
2. **Comportamiento más predecible.** La legacy escala como
   `O((ub-lb+1) × ub)` en pasos de join; la incremental como `O(ub)`.
   Para queries con rangos grandes (`{1,10}`, `{1,20}`) la legacy se
   degrada cuadráticamente y la incremental linealmente. No tenemos
   queries de ese tamaño en el suite hoy, pero sí está en los planes
   (rangos típicos de path-finding).
3. **El código incremental no es más complejo que el legacy.** Las dos
   versiones tienen ~35 líneas cada una. El incremental usa un truco
   (acumulador único con `level_ranges`) pero el invariante es claro
   y los tests existentes lo cubren.

**Qué eliminar y qué conservar:**

- Eliminar: el match arm que dispatcha por env var (`engine.rs:418-433`)
  y la función `repeat_incremental_enabled`.
- Conservar dentro de `run_repetition_range`: la llamada a
  `run_repetition_pattern(p, 0)` para el caso `lb=0` (es la baseline
  de todos los nodos; duplicar `fill_empty_list` aquí no aporta).
- Conservar `run_repetition_pattern` solo si decidimos que el caso
  `n=0` se maneja desde ahí. Si quitamos esa dependencia (inlineando
  el baseline), la función puede borrarse entera y bajamos ~50 líneas.

**Lo que falta antes de mergear:**

- Hacer el incremental el default sin el flag.
- Decidir si `run_repetition_pattern` se elimina o queda como helper
  para `n=0`.
- Eliminar `bench_repeat.rs` o moverlo a `bench/scripts/` si querés
  mantenerlo como regresión periódica (no es parte del CI hoy).

**Caso para conservar el flag transitoriamente:**

- Solo si querés correr A/B en datasets reales (LDBC SF1, soc-LiveJournal1)
  antes de mergear. El bench sintético cubre la dinámica algorítmica
  pero no el costo de I/O sobre `LazyGraphStore`. Si el plan es mergear
  ya y revertir si aparece regresión en LDBC, el flag se puede sacar.
