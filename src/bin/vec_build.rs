//! Offline builder for a vector-attribute sidecar.
//!
//! Everything a vector query needs is constructed here, before any
//! measurement: the vectors are laid out in row order, the HNSW graph is
//! built, and both are written to `<db>.vec.<attr>`. Query time then only
//! reads. That split is deliberate — the point of the experiment is to
//! compare evaluation strategies, so index construction must never land
//! inside a measured query.
//!
//! ```text
//! vec_build <db.gdb> --attr <name> --input <vectors.csv> [options]
//! vec_build <db.gdb> --attr <name> --random 128          [options]
//!
//!   --attr <name>            vector attribute name (required)
//!   --input <path>           CSV: key,v0,v1,...  one row per node
//!   --random <dim>           instead of --input, give every node a
//!                            pseudo-random unit-cube vector of this
//!                            dimension (synthetic benchmark data)
//!   --key internal|name|<Label>.<prop>
//!                            how the CSV key column maps to a node
//!                            (default: internal, the raw u32 node id)
//!   --metric l2|cosine|ip    distance metric (default: l2)
//!   --m <n>                  HNSW links per node above layer 0 (16)
//!   --ef-construction <n>    HNSW build candidate width (200)
//!   --seed <n>               PRNG seed for layers and --random
//!   --no-index               store vectors only, no HNSW graph
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use frogql::model::graph_access::GraphAccess;
use frogql::model::value::Value;
use frogql::store::lazy::LazyGraphStore;
use frogql::vector::hnsw::{Hnsw, HnswParams};
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

/// How a key in the input file names a node.
enum KeyMode {
    /// The key is the graph-internal u32 node id.
    Internal,
    /// The key is the node's user-facing name.
    Name,
    /// The key is the value of `(label, prop)`, resolved through the
    /// secondary index when there is one and a scan otherwise.
    Prop(String, String),
}

struct Args {
    db: PathBuf,
    attr: String,
    input: Option<PathBuf>,
    random_dim: Option<usize>,
    key: KeyMode,
    metric: Metric,
    params: HnswParams,
    build_index: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: vec_build <db.gdb> --attr <name> (--input <csv> | --random <dim>) [options]\n\
         \n\
         options:\n  \
           --key internal|name|<Label>.<prop>   key column meaning (default: internal)\n  \
           --metric l2|cosine|ip                distance metric (default: l2)\n  \
           --m <n>                              HNSW links above layer 0 (default: 16)\n  \
           --ef-construction <n>                HNSW build width (default: 200)\n  \
           --seed <n>                           PRNG seed\n  \
           --no-index                           vectors only, skip the HNSW build"
    );
    process::exit(2)
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        usage();
    }
    let mut db = None;
    let mut attr = None;
    let mut input = None;
    let mut random_dim = None;
    let mut key = KeyMode::Internal;
    let mut metric = Metric::L2Sq;
    let mut params = HnswParams::default();
    let mut build_index = true;

    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let mut value = |name: &str| -> String {
            i += 1;
            match argv.get(i) {
                Some(v) => v.clone(),
                None => {
                    eprintln!("error: {name} needs a value");
                    usage()
                }
            }
        };
        match a.as_str() {
            "--attr" => attr = Some(value("--attr")),
            "--input" => input = Some(PathBuf::from(value("--input"))),
            "--random" => random_dim = Some(parse_usize(&value("--random"), "--random")),
            "--metric" => {
                let v = value("--metric");
                metric = match Metric::parse(&v) {
                    Some(m) => m,
                    None => {
                        eprintln!("error: unknown metric `{v}` (want l2, cosine, or ip)");
                        usage()
                    }
                };
            }
            "--key" => {
                let v = value("--key");
                key = match v.as_str() {
                    "internal" => KeyMode::Internal,
                    "name" => KeyMode::Name,
                    other => match other.split_once('.') {
                        Some((l, p)) if !l.is_empty() && !p.is_empty() => {
                            KeyMode::Prop(l.to_string(), p.to_string())
                        }
                        _ => {
                            eprintln!("error: --key wants `internal`, `name`, or `Label.prop`");
                            usage()
                        }
                    },
                };
            }
            "--m" => params.m = parse_usize(&value("--m"), "--m"),
            "--ef-construction" => {
                params.ef_construction =
                    parse_usize(&value("--ef-construction"), "--ef-construction")
            }
            "--seed" => params.seed = parse_usize(&value("--seed"), "--seed") as u64,
            "--no-index" => build_index = false,
            "-h" | "--help" => usage(),
            other if other.starts_with('-') => {
                eprintln!("error: unknown flag `{other}`");
                usage()
            }
            other => {
                if db.is_some() {
                    eprintln!("error: unexpected positional argument `{other}`");
                    usage();
                }
                db = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }

    let db = match db {
        Some(d) => d,
        None => {
            eprintln!("error: no database path given");
            usage()
        }
    };
    let attr = match attr {
        Some(a) => a,
        None => {
            eprintln!("error: --attr is required");
            usage()
        }
    };
    if input.is_some() == random_dim.is_some() {
        eprintln!("error: give exactly one of --input or --random");
        usage();
    }

    Args {
        db,
        attr,
        input,
        random_dim,
        key,
        metric,
        params,
        build_index,
    }
}

fn parse_usize(s: &str, flag: &str) -> usize {
    match s.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("error: {flag} wants a non-negative integer, got `{s}`");
            process::exit(2)
        }
    }
}

/// Resolve input keys to graph-internal node ids.
struct Resolver {
    mode: KeyMode,
    /// Built lazily for `KeyMode::Name`, and for `KeyMode::Prop` when the
    /// database has no secondary index on `(label, prop)`.
    by_name: Option<HashMap<String, u32>>,
}

impl Resolver {
    fn new(mode: KeyMode) -> Resolver {
        Resolver {
            mode,
            by_name: None,
        }
    }

    fn resolve(&mut self, store: &LazyGraphStore, key: &str) -> Result<u32, String> {
        match &self.mode {
            KeyMode::Internal => key
                .parse::<u32>()
                .map_err(|_| format!("key `{key}` is not a u32 node id")),
            KeyMode::Name => {
                if self.by_name.is_none() {
                    let mut map = HashMap::new();
                    for id in store.nodes() {
                        map.insert(store.node_name(id).to_string(), id);
                    }
                    self.by_name = Some(map);
                }
                match self.by_name.as_ref().and_then(|m| m.get(key)) {
                    Some(id) => Ok(*id),
                    None => Err(format!("no node named `{key}`")),
                }
            }
            KeyMode::Prop(label, prop) => {
                // Try the key both ways: LDBC-style ids are integers, but
                // the property may equally be a string.
                let candidates: Vec<Value> = match key.parse::<i64>() {
                    Ok(n) => vec![Value::Int(n), Value::Str(key.to_string())],
                    Err(_) => vec![Value::Str(key.to_string())],
                };
                for v in &candidates {
                    if let Some(hits) = store.lookup_node_eq(label, prop, v) {
                        match hits.len() {
                            0 => continue,
                            1 => return Ok(hits[0]),
                            n => {
                                return Err(format!(
                                    "key `{key}` matches {n} nodes on {label}.{prop}; \
                                     the key must be unique"
                                ))
                            }
                        }
                    }
                }
                Err(format!(
                    "no node with {label}.{prop} = `{key}` (is there an index on it?)"
                ))
            }
        }
    }
}

/// Parse `key,v0,v1,...` rows. Blank lines and `#` comments are skipped,
/// and a leading header line is detected by its second field failing to
/// parse as a float.
fn read_csv(path: &Path) -> Result<Vec<(String, Vec<f32>)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut out: Vec<(String, Vec<f32>)> = Vec::new();
    let mut dim: Option<usize> = None;

    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split(',');
        let key = match fields.next() {
            Some(k) => k.trim().to_string(),
            None => continue,
        };
        let rest: Vec<&str> = fields.map(|f| f.trim()).collect();
        if rest.is_empty() {
            return Err(format!(
                "{}:{}: row has a key but no vector components",
                path.display(),
                lineno + 1
            ));
        }
        let parsed: Result<Vec<f32>, _> = rest.iter().map(|f| f.parse::<f32>()).collect();
        let vec = match parsed {
            Ok(v) => v,
            Err(_) if out.is_empty() && dim.is_none() => continue, // header row
            Err(e) => {
                return Err(format!(
                    "{}:{}: bad vector component ({e})",
                    path.display(),
                    lineno + 1
                ))
            }
        };
        match dim {
            None => dim = Some(vec.len()),
            Some(d) if d != vec.len() => {
                return Err(format!(
                    "{}:{}: row has {} components, earlier rows have {d}",
                    path.display(),
                    lineno + 1,
                    vec.len()
                ))
            }
            Some(_) => {}
        }
        out.push((key, vec));
    }
    Ok(out)
}

/// Same xorshift64* as the HNSW layer draw. Deterministic synthetic data
/// keeps a benchmark run reproducible.
fn random_rows(store: &LazyGraphStore, dim: usize, seed: u64) -> Vec<(u32, Vec<f32>)> {
    let mut state = if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    };
    let mut next = move || {
        let mut x = state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        state = x;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((r >> 11) as f64 / 9_007_199_254_740_992.0) as f32 * 2.0 - 1.0
    };
    let mut nodes = store.nodes();
    nodes.sort_unstable();
    nodes
        .into_iter()
        .map(|id| (id, (0..dim).map(|_| next()).collect()))
        .collect()
}

fn main() {
    let args = parse_args();

    let open_start = Instant::now();
    let store = match LazyGraphStore::open(&args.db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", args.db.display());
            process::exit(1);
        }
    };
    eprintln!(
        "opened {} ({} nodes, {} edges) in {:?}",
        args.db.display(),
        store.node_count(),
        store.edge_count(),
        open_start.elapsed()
    );

    // Collect (internal id, vector) pairs.
    let mut rows: Vec<(u32, Vec<f32>)> = match args.random_dim {
        Some(dim) => random_rows(&store, dim, args.params.seed),
        None => {
            let path = args.input.as_ref().expect("checked in parse_args");
            let raw = match read_csv(path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    process::exit(1);
                }
            };
            let mut resolver = Resolver::new(args.key);
            let mut out = Vec::with_capacity(raw.len());
            let mut unresolved = 0usize;
            let mut first_error = None;
            for (key, vec) in raw {
                match resolver.resolve(&store, &key) {
                    Ok(id) => out.push((id, vec)),
                    Err(e) => {
                        unresolved += 1;
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                    }
                }
            }
            if unresolved > 0 {
                // Loud, not fatal: a partial attribute is legitimate (not
                // every node need carry a vector), but silently dropping
                // rows would quietly shrink the search space.
                eprintln!(
                    "warning: {unresolved} input row(s) did not resolve to a node; first: {}",
                    first_error.unwrap_or_default()
                );
            }
            out
        }
    };

    if rows.is_empty() {
        eprintln!("error: no vectors to write");
        process::exit(1);
    }

    // The sidecar requires strictly ascending ids.
    rows.sort_by_key(|(id, _)| *id);
    if let Some(w) = rows.windows(2).find(|w| w[0].0 == w[1].0) {
        eprintln!(
            "error: node {} appears twice in the input; each node may carry at most \
             one vector per attribute",
            w[0].0
        );
        process::exit(1);
    }

    let dim = rows[0].1.len();
    if let Some((id, v)) = rows.iter().find(|(_, v)| v.len() != dim) {
        eprintln!(
            "error: node {id} has a {}-component vector, others have {dim}",
            v.len()
        );
        process::exit(1);
    }
    let max_id = rows[rows.len() - 1].0;
    if max_id >= store.node_count() {
        eprintln!(
            "error: node id {max_id} is beyond the graph's {} nodes",
            store.node_count()
        );
        process::exit(1);
    }

    let ids: Vec<u32> = rows.iter().map(|(id, _)| *id).collect();
    let data: Vec<f32> = rows.into_iter().flat_map(|(_, v)| v).collect();
    let fp = fingerprint(store.node_count() as usize, store.edge_count() as usize);
    let set = VectorSet::new(args.attr.clone(), dim, args.metric, fp, ids, data);

    let set = if args.build_index {
        let t = Instant::now();
        let graph = Hnsw::build(&set, args.params);
        eprintln!(
            "built HNSW over {} rows (m={}, ef_construction={}) in {:?}",
            set.len(),
            args.params.m,
            args.params.ef_construction,
            t.elapsed()
        );
        set.with_hnsw(graph)
    } else {
        eprintln!("skipping the HNSW build (--no-index)");
        set
    };

    let path = Sidecar::path_for(&args.db, &args.attr);
    if let Err(e) = set.to_sidecar().write_to_path(&path) {
        eprintln!("error: cannot write {}: {e}", path.display());
        process::exit(1);
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {} — attr={} rows={} dim={} metric={} index={} fingerprint={:#x} size={:.1} MiB",
        path.display(),
        args.attr,
        set.len(),
        dim,
        args.metric.name(),
        set.has_index(),
        fp,
        bytes as f64 / (1024.0 * 1024.0)
    );
}
