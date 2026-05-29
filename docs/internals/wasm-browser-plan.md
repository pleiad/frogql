# Plan: froGQL en el navegador vía WebAssembly

## Objetivo

Ejecutar froGQL client-side en JavaScript/TypeScript, sin servidor, compilando el
crate a WebAssembly. El alcance se divide en fases para entregar valor temprano y
dejar el trabajo caro (paginación sobre disco) para cuando un grafo real lo
justifique.

> **Estado.** Fase 1 completa (1.1–1.4). El crate `wasm/` (`frogql-wasm`) compila a
> `wasm32-unknown-unknown` y expone `open_json` / `execute` / `to_json` / `schema`
> sobre `MemoryGraphStore`. El núcleo (`query_json` / `dm_json`) tiene tests en host
> (`cargo test -p frogql-wasm`). **Empaquetado (1.3):** `playground/frontend/scripts/build-wasm.sh`
> (vía `npm run build:wasm`) corre `cargo build --target wasm32` + `wasm-bindgen
> --target web` y genera `src/frogql-wasm/` (gitignored). **Persistencia:** IndexedDB
> en `src/engine/indexeddb.ts` guarda el `to_json()`. **Playground (1.4):** toggle
> Server/Browser en `usePlayground`, motor WASM en `src/engine/wasmEngine.ts`.
> Verificado end-to-end en navegador (Playwright): cargar grafo, ejecutar query
> (`MATCH (n:Person) RETURN n.name` → Alice/Bob), guardar y recargar desde IndexedDB.
> Nota de implementación: usar `serde_wasm_bindgen::Serializer::json_compatible()`
> (no `to_value`), si no los objetos llegan a JS como `Map` vacíos. Pendiente:
> Fase 2 (OPFS). Ver `wasm/README.md`.

## Decisión de arquitectura

El binding nativo `node/` (napi-rs) corre solo en Node.js, no en el navegador. Para
el browser, WASM es el único camino. El subconjunto del crate que compila a
`wasm32-unknown-unknown` es el backend `MemoryGraphStore` en memoria:

| Componente | WASM en browser | Motivo |
|---|---|---|
| `MemoryGraphStore::from_json_value` / `from_json_str` | sí | sin filesystem, todo en RAM |
| `compile_query_with_diagnostics_with` → `Runtime::run_query` | sí | lógica pura |
| `infer_simple_schema::<MemoryGraphStore>` | sí | genérico sobre `GraphAccess` |
| LTJ `TripleIndex` | sí | se construye en memoria |
| DML sobre `MemoryGraphStore` (`GraphAccessMut`) | sí | implementa `GraphAccessMut` con el mismo `MutationOverlay` que `LazyGraphStore`; INSERT/SET/REMOVE/DELETE funcionan en RAM (parity en `tests/memory_mut_test.rs`) |
| `LazyGraphStore` / `DiskGraphStore` / `Pager` | no | `pager.rs`, `io.rs`, `string_table.rs` usan `std::fs` |
| Trazas `Instant::now()` en `lazy.rs:180+` | no | hace panic en `wasm32-unknown-unknown`; solo en el path de `LazyGraphStore::open`, que la Fase 1 no toca |

La Connection de WASM espeja la de Python (`python/src/lib.rs`): dueña del `MemoryGraphStore` y
del `Arc<TripleIndex>` cacheado, construye un `Runtime` fresco por `execute` que toma
prestado el `MemoryGraphStore` propio. Eso evita el problema de struct autorreferencial que
introduciría el lifetime `'g` de `Runtime<'g, G>`.

---

## Fase 1: WASM + MemoryGraphStore en memoria + snapshot en IndexedDB

Meta: demo interactiva sin servidor que persiste el grafo entre sesiones. Cubre
grafos de tamaño moderado que caben en RAM.

### 1.1 Crate `wasm/` (cuarto miembro del workspace)

- Agregar `wasm` a `members` en `Cargo.toml` raíz (junto a `python`, `node`).
- `wasm/Cargo.toml`:
  ```toml
  [lib]
  crate-type = ["cdylib"]

  [dependencies]
  gqlrust = { path = "..", default-features = false }
  wasm-bindgen = "0.2"
  serde-wasm-bindgen = "0.6"
  console_error_panic_hook = "0.1"   # panics legibles en la consola del browser
  ```
- `default-features = false` replica la disciplina de `python/` y `node/`: nada de
  `rustyline`, `ureq`, `zstd`, `tar`, `sysinfo`, `toml`. Con `resolver = "2"` ya
  activo, el build de WASM no arrastra esas deps.

### 1.2 Superficie `#[wasm_bindgen]` (`wasm/src/lib.rs`)

Espejo de la API de Python, camelCase no es necesario (wasm-bindgen respeta nombres):

- `open_json(json: &str) -> Connection` — parsea con `MemoryGraphStore::from_json_str`, infiere
  el schema DEFAULT con `infer_simple_schema`, calienta el `TripleIndex` una vez.
- `Connection.execute(query: &str, limit: usize) -> JsValue` — compila con
  `compile_query_with_diagnostics_with(&schema, query)`, construye `Runtime` fresco
  sobre el `MemoryGraphStore` propio reusando el `Arc<TripleIndex>`, corre `run_query`, serializa
  filas con `serde_wasm_bindgen::to_value`.
- `Connection.execute_dm(...)` — opcional en esta fase; `MemoryGraphStore` implementa
  `GraphAccessMut`, así que INSERT/SET/DELETE funcionan en RAM. Invalidar el
  `TripleIndex` cacheado tras cada DML (igual que `Runtime::invalidate_caches`).
- `Connection.schema() -> JsValue`, `Connection.node_count`, `edge_count`.
- `Connection.to_json() -> String` — serializa el `MemoryGraphStore` (incluido cualquier DML
  aplicado) para persistir.

### 1.3 Capa JS/TS de persistencia

- IndexedDB, no localStorage. localStorage tope ~5 MB, síncrono y solo strings; sirve
  solo para metadatos (nombre del grafo activo, última query).
- Wrapper TS:
  - `saveGraph(name, connection)` → `connection.to_json()` → guardar como `Blob` en
    IndexedDB.
  - `loadGraph(name)` → leer `Blob` → `open_json(text)`.
- Empaquetado: `wasm-pack build --target bundler` (compatible con el Vite que ya usa
  `playground/frontend`). El `.wasm` y el glue JS quedan importables como módulo ES.

### 1.4 Integración con el playground

- Modo client-side opcional en `playground/frontend`: el `usePlayground` hook puede
  rutear `execute` al módulo WASM en vez de a `/api/execute`. Reusa CodeMirror y
  `lang-gql.ts` sin cambios.

### 1.5 Verificación Fase 1

- Smoke test en el browser: cargar un `examples/*.gdb` exportado a JSON, correr las
  queries de `tests/runtime_test.rs` representativas, comparar contra el binding
  nativo.
- Confirmar que `console_error_panic_hook` reporta panics legibles.
- Medir tamaño del `.wasm` (objetivo < 2 MB con `opt-level = "z"` y `wasm-opt`).

---

## Fase 2 (diferida): Pager genérico sobre OPFS

Meta: grafos que no caben en RAM, con paginación bajo demanda en el navegador.
Conserva la ventaja de `LazyGraphStore`. Solo abordar cuando la Fase 1 quede corta
por tamaño.

### 2.1 Abstraer el backend de IO del `Pager`

- Hoy `Pager` tiene `file: File` y todo el IO pasa por `read_page` /
  `read_page_from_disk` (`pager.rs:94`, `:220`). Hacer `Pager` genérico sobre un trait
  `PageStore` con métodos `read_at(offset, buf)` / `write_at(offset, buf)` /
  `len()` / `flush()`.
- Variante nativa: `FilePageStore` envuelve `std::fs::File` (comportamiento actual,
  detrás de feature por defecto).
- Variante WASM: `OpfsPageStore` envuelve un `FileSystemSyncAccessHandle`, cuyos
  `read({at})` / `write({at})` mapean uno a uno a `seek` + `read`/`write`.
- Eliminar o gatear los `Instant::now()` de `lazy.rs` detrás de `cfg(not(wasm))` o un
  reloj inyectable, porque hacen panic en `wasm32-unknown-unknown`.

### 2.2 Modelo de ejecución en Web Worker

- El acceso síncrono de OPFS solo existe dentro de un Web Worker. El módulo WASM corre
  en el worker; la UI se comunica por `postMessage`.
- Seguir la variante `opfs-sahpool` de SQLite WASM: SyncAccessHandle Pool, sin requerir
  cabeceras COOP/COEP, compatible desde Safari 16.4. Contra: una sola conexión a la
  vez (sin concurrencia entre pestañas), aceptable porque froGQL no la tiene hoy.
- Evitar la variante OPFS estándar: exige COOP/COEP (SharedArrayBuffer), lo que obliga
  control del servidor y rompe despliegues simples como GitHub Pages.

### 2.3 Verificación Fase 2

- Abrir un `.gdb` del tamaño de `bench/data/ldbc-sf0.1` desde OPFS, confirmar que la
  RAM se mantiene acotada (no se materializa el grafo completo).
- Comparar latencia de query contra el modo snapshot de la Fase 1.

---

## Alternativa para grafos grandes de producción

Para grafos del tamaño de los benchmarks LDBC, ninguna opción de browser compite con
froGQL nativo. El camino de producción es el binding nativo (Node `frogql` o un
servicio Rust) detrás de una API HTTP, como ya hace `playground/backend` con pygql. El
frontend queda en JS/TS puro llamando a `/api/execute`. Esta vía y la WASM no se
excluyen: el playground puede ofrecer ambos modos.

---

## Riesgos y notas

- **Tamaño del bundle.** El `.wasm` compite con el grafo por memoria del browser.
  Mitigar con `opt-level = "z"`, `lto`, `wasm-opt -Oz`.
- **`HashMap` en wasm32-unknown-unknown.** El `RandomState` de std se siembra desde un
  contador, no desde `getrandom`; no hace panic. Sin dependencia extra.
- **Compatibilidad OPFS (solo Fase 2).** Acceso síncrono desde Chrome/Edge 102+,
  Firefox 111+, Safari 16.4+. El piso real es Safari 16.4.
- **Modo incógnito.** OPFS puede tener cuota estricta o nula; IndexedDB también. La UI
  debe degradar a sesión efímera sin romperse.

## Orden de trabajo sugerido

1. Crate `wasm/` + `open_json` / `execute` / `to_json` mínimos (Fase 1.1–1.2).
2. Wrapper TS de IndexedDB + `wasm-pack build --target bundler` (Fase 1.3).
3. Modo client-side opcional en el playground (Fase 1.4) + verificación (Fase 1.5).
4. (Diferido) `PageStore` trait + `OpfsPageStore` + Web Worker (Fase 2).
