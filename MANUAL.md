# GQL Rust — Manual de uso

## Qué es

Motor de base de datos de grafos que implementa GQL (Graph Query Language) con path pattern matching. Soporta:

- Almacenamiento en un solo archivo (`.gql`) inspirado en SQLite
- Carga desde JSON para prototipos rápidos
- Compilación de queries con optimización automática
- Pattern matching con nodos, aristas, concatenación, unión, filtros, repetición

## Instalación

```bash
cd gqlrust
cargo build --release
```

## Formato de datos (JSON)

El grafo se define con nodos y aristas. Cada nodo/arista puede tener **múltiples labels** y propiedades tipadas (int, str, bool).

```json
{
  "nodes": [
    {
      "id": "a1",
      "labels": ["Account"],
      "props": {
        "owner": "Aretha",
        "isBlocked": false
      }
    },
    {
      "id": "d1",
      "labels": ["Dummy", "Person"],
      "props": {
        "owner": "Fred",
        "isDummy": true
      }
    }
  ],
  "edges": [
    {
      "id": "t1",
      "labels": ["Transfer"],
      "props": { "amount": 2500000 },
      "endpoints": ["a1", "a2"],
      "directionality": "->"
    },
    {
      "id": "e1",
      "labels": ["Knows"],
      "props": { "since": 2020 },
      "endpoints": ["n1", "n2"],
      "directionality": "~~"
    }
  ]
}
```

| Campo | Descripción |
|-------|-------------|
| `id` | Identificador único (string) |
| `labels` | Lista de labels (al menos uno). Nodos/aristas pueden tener múltiples |
| `props` | Mapa de propiedades. Valores: string, int, o bool |
| `endpoints` | Solo aristas: `[origen, destino]` |
| `directionality` | `"->"` (dirigida) o `"~~"` (no dirigida) |

## Uso desde Rust

### Pipeline de ejecución

```
compile(query)  →  runtime.run(pattern)
   ↑ compilación       ↑ ejecución
   (parse + optimize)  (evalúa contra el grafo)
```

### Ejemplo completo

```rust
use std::path::Path;
use gqlrust::compile;
use gqlrust::model::graph::Graph;
use gqlrust::runtime::engine::Runtime;

// 1. Cargar grafo desde JSON
let graph = Graph::from_file(Path::new("datos.json")).unwrap();

// 2. Crear runtime
let rt = Runtime::new(&graph);

// 3. Compilar query (parse + optimización)
let pattern = compile("(x: Account)-[:Transfer]->(y)").unwrap();

// 4. Ejecutar
let results = rt.run(&pattern);

// 5. Leer resultados
for row in &results.rows {
    println!("Path: {}", row.path);
    println!("  Variables: {}", row.assignment);
}
println!("Total: {} resultados", results.rows.len());
```

### Con almacenamiento persistente (.gql)

```rust
use gqlrust::store::graph_store::GraphStore;

// Importar JSON a archivo .gql
let store = GraphStore::from_json_file(
    Path::new("mi_base.gql"),
    Path::new("datos.json"),
).unwrap();

// Abrir base existente
let store = GraphStore::open(Path::new("mi_base.gql")).unwrap();

// Ejecutar queries (misma interfaz que Graph)
let rt = Runtime::new(&store);
let pattern = compile("(x: Person)-[:Knows]->(y)").unwrap();
let results = rt.run(&pattern);
```

## Lenguaje de queries GQL

### Patrones de nodos

```
()                        Cualquier nodo
(x)                       Cualquier nodo, bound a variable x
(x: Account)              Nodo con label Account
(:Person & Teacher)       Nodo con ambos labels
(x: Account {owner: str}) Nodo con label y propiedad tipada
(:{isDummy: bool})        Nodo con propiedad (sin variable)
```

### Patrones de aristas

```
->                         Arista dirigida (cualquiera)
-[]->                      Equivalente a ->
-[:Transfer]->             Arista dirigida con label Transfer
-[e: Transfer]->           Bound a variable e
-[e: Transfer {amount: int}]->   Con label y propiedad
<-[:Label]-                Arista dirigida en reversa
~[:Knows]~                 Arista no dirigida
-[:Label]-                 Arista en cualquier dirección
```

**Importante:** El label va después de `:` — `-[:Transfer]->`, no `-[Transfer]->`.

### Concatenación

Los patrones se concatenan por yuxtaposición:

```
(x: Account)-[:Transfer]->(y: Account)
(x)-[]->(y)-[]->(z)
```

La concatenación conecta paths donde el último nodo del izquierdo coincide con el primer nodo del derecho.

### Unión

```
(x: Person) | (y: Company)
```

Retorna resultados de ambos patrones. Variables no compartidas se llenan con `Nothing`.

### Filtros (WHERE)

```
(x WHERE x.isBlocked = true)
(x WHERE x.amount > 1000000)
(x WHERE x.isBlocked is bool)
(x WHERE x.name as str)
(x WHERE not x.isBlocked)
(x WHERE -x.amount < 0)
((x)-[y]->(z) WHERE x.owner = 'Jay' and y.amount > 100)
```

**Operadores:**

| Tipo | Operadores |
|------|-----------|
| Comparación | `=`, `!=`, `<`, `>`, `<=`, `>=` |
| Lógicos | `and`, `or`, `not` |
| Aritméticos | `+`, `-` (unario y binario) |
| Tipo | `is` (test de tipo), `as` (cast) |

**Tipos:** `int`, `bool`, `str`, `*` (cualquiera)

### Repetición

```
(-->){1,3}                 De 1 a 3 repeticiones
(-[:Transfer]->){2,4}      Transfers encadenados, 2 a 4 hops
(x)*                       0 o más (no soportado en runtime)
(x)+                       1 o más (no soportado en runtime)
(x){3}                     Exactamente 3
(x){2,}                    2 o más (no soportado en runtime)
```

**Nota:** Solo repetición acotada `{lb, ub}` está implementada en el runtime.

### Labels múltiples

```
(:Person & Teacher)        Nodo con AMBOS labels
(:Person | Company)        Nodo con ALGUNO de los labels
(:!Admin)                  Nodo que NO tiene el label Admin
```

En el JSON, un nodo con `"labels": ["Person", "Teacher"]` tiene el tipo `Person & Teacher` y matchea con `(:Person)`, `(:Teacher)`, o `(:Person & Teacher)`.

### Propiedades abiertas vs cerradas

```
(x: {name: str})           Abierto: tiene name:str, puede tener más
(x: {{name: str}})         Cerrado: tiene EXACTAMENTE name:str, nada más
```

## Optimizaciones

El compilador aplica automáticamente:

1. **Predicate pushdown**: Extrae constraints de tipo del WHERE y los mueve a los descriptores:
   ```
   ((x)-[y]->(z) WHERE x.a is bool and y.b is str)
   →  (x: {a: bool})-[y: {b: str}]->(z)
   ```
   Solo funciona con conjunciones (`and`). Con `or` el filtro permanece.

2. **Label index**: Cuando un patrón tiene un label simple como `(x: Account)`, usa un índice invertido en vez de escanear todos los nodos.

3. **Adjacency-driven concat**: En concatenaciones como `(x)-[:Transfer]->(y)`, usa listas de adyacencia del nodo para encontrar aristas conectadas, en vez de hacer producto cruzado.

## Resultados

Cada resultado es un `ResultRow` con:
- `path`: Secuencia de nodos y aristas matcheados (ej: `a1 t1 a2`)
- `assignment`: Mapa de variables a valores (ej: `x ↦ a1, y ↦ a2`)

```rust
for row in &results.rows {
    // Acceder al path
    for elem in &row.path.0 {
        match elem {
            PathValue::Node(id) => println!("Nodo: {id}"),
            PathValue::EdgeDirectional(id) => println!("Arista →: {id}"),
            PathValue::EdgeUndirectional(id) => println!("Arista ~: {id}"),
            _ => {}
        }
    }

    // Acceder a variables
    if let Some(val) = row.assignment.get("x") {
        println!("x = {val}");
    }
}
```

## Tests

```bash
cargo test                    # Todos los tests (189)
cargo test --test parser_test # Solo tests del parser
cargo test --test bench_test --release -- --nocapture  # Benchmarks
```
