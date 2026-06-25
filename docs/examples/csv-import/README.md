# Cargar un CSV en froGQL desde Python

Guía para empezar a usar froGQL cargando datos desde archivos CSV. Esta
carpeta contiene un ejemplo mínimo y funcional: cópiala y modifícala.

```
docs/examples/csv-import/
├── spanner_import_config.json   # describe los CSV
├── Person.csv                   # nodos Person
├── Movie.csv                    # nodos Movie
└── acted_in.csv                 # aristas ACTED_IN
```

## 1. Instalar

```bash
pip install frogql
```

## 2. El `spanner_import_config.json`

Un objeto con un único arreglo `files`. Cada entrada describe un CSV: su
`path` (relativo a esta carpeta) y un mapa `columns` de `nombre → tipo`. El
archivo de config debe llamarse exactamente `spanner_import_config.json` y
estar junto a los CSV.

```json
{
  "files": [
    {
      "path": "Person.csv",
      "columns": { "vid": "STRING", "name": "STRING", "age": "INT64" }
    },
    {
      "path": "Movie.csv",
      "columns": { "vid": "STRING", "title": "STRING", "year": "INT64" }
    },
    {
      "path": "acted_in.csv",
      "label": "ACTED_IN",
      "columns": { "SRC_ID": "STRING", "DST_ID": "STRING", "role": "STRING" }
    }
  ]
}
```

## 3. La regla clave: nodo vs arista

El loader decide el tipo de cada archivo por sus columnas:

- **Arista**: si `columns` tiene **`SRC_ID` y `DST_ID`** (insensible a mayúsculas).
- **Nodo**: cualquier archivo que no los tenga.

## 4. Archivos de nodo

- **El label del nodo viene del nombre del archivo**, no del config.
  `Person.csv` produce nodos con label `Person`. Nombra el CSV igual al
  label que quieres.
- **Columna de ID**: el loader la busca en este orden: `vid` →
  `<label>_id` → cualquier columna que termine en `_id` → la primera
  columna. Lo más simple es llamarla `vid`.
- El resto de columnas se vuelven propiedades.

`Person.csv`:
```csv
vid,name,age
p1,Ana,30
p2,Beto,25
p3,Carla,41
```

## 5. Archivos de arista

- `SRC_ID` y `DST_ID` contienen los **valores de ID** (los `vid`) de los
  nodos origen y destino.
- El **label de la arista** sale del campo `"label"` del config (o del
  nombre del archivo si no lo pones).
- Columnas que no sean `SRC_ID`, `DST_ID` ni `vid` se vuelven propiedades.
- Todas las aristas son dirigidas (SRC → DST).

`acted_in.csv`:
```csv
SRC_ID,DST_ID,role
p1,m1,Cobb
p2,m2,Neo
p3,m1,Mal
```

## 6. Tipos de columna admitidos

| Token en el config | Tipo resultante |
|---|---|
| `INT64` | entero |
| `FLOAT64`, `DOUBLE`, `FLOAT` | flotante |
| `BOOL` | booleano (`true` o `1`) |
| `STRING` (o cualquier otro) | texto (si empieza con `[` o `{` intenta decodificar JSON a lista/record) |

## 7. Cargar y consultar desde Python

```python
import frogql

# Construye el .gdb leyendo spanner_import_config.json de la carpeta.
frogql.import_csv("social.gdb", "docs/examples/csv-import/")

# Abre la base y consulta.
conn = frogql.open("social.gdb")

rows = conn.execute(
    "MATCH (p:Person)-[:ACTED_IN]->(m:Movie) RETURN p.name AS name, m.title AS title",
    limit=10,
)
for r in rows:
    print(r)          # {'name': 'Ana', 'title': 'Inception'}

print(conn.node_count, conn.edge_count)   # 5 3
```

Cada fila es un `dict`. Usa alias con `AS` para nombrar las columnas; sin
alias, las claves caen a `col0`, `col1`, …


## 8. Errores típicos

- **El label del nodo es el nombre del archivo.** Para nodos `Person`, el
  archivo debe llamarse `Person.csv`. No hay campo de label para nodos.
- **Las aristas con extremos desconocidos se descartan en silencio.** Si un
  `SRC_ID` o `DST_ID` no coincide con ningún `vid` cargado, esa fila se
  omite sin error. Carga primero bien los nodos.
- **El label de arista puede recortarse.** El loader quita nombres de tipos
  de nodo del inicio y fin del label. Con tipos `Person`/`Movie`, un label
  `ACTED_IN` queda intacto; evita labels que empiecen o terminen con un
  nombre de nodo.
- **CSV separado por comas**, primera fila encabezados, comillas dobles con
  `""` para escapar. Valores vacíos se omiten (la propiedad no se crea). Las
  columnas se buscan sin distinguir mayúsculas.
- **En los patrones, los labels llevan `:`**: `(p:Person)`, `-[:ACTED_IN]->`.

## Verificar el ejemplo sin Python (REPL)

```bash
frogql social.gdb --import-csv docs/examples/csv-import/
# luego, en el prompt:
MATCH (p:Person)-[:ACTED_IN]->(m:Movie) RETURN p.name, m.title
```
