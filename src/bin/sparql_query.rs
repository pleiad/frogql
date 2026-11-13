//! Run one SPARQL `SELECT` query against a `.gdb` and print the rows.
//!
//! ```text
//! sparql_query <db.gdb> <query.rq | -> [options]
//!
//!   --import-ttl [<label>:<prop>]  the layout import_ttl writes (default img:id):
//!                                  local-name edge labels, integer key, one node label
//!   --local-names                  edge label = predicate's local name (default: whole IRI)
//!   --node-label <label>           label every resource node carries
//!   --key <prop>                   node property identifying a resource (default: iri)
//!   --int-key                      the key holds the IRI's integer local name
//!   --limit <n>                    stop after n rows (default: no cap)
//!   --explain                      print the GQL the query translates to (a constant
//!                                  IRI's key filter is not shown: `Descriptor`'s
//!                                  Display omits value filters)
//!   --gql                          the file is GQL, not SPARQL (for an A/B on the same db)
//! ```
//!
//! The SPARQL frontend's design: `docs/internals/sparql-frontend.md`.

use std::io::Read;
use std::process::exit;
use std::time::Instant;

use frogql::sparql::{translate, NodeKey, PredicateNaming, SparqlMapping};
use frogql::Connection;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: sparql_query <db.gdb> <query.rq | -> [--import-ttl [label:prop]] [--local-names] [--node-label L] [--key P] [--int-key] [--limit N] [--explain] [--gql]");
        exit(2);
    }
    let db = &args[0];
    let text = if args[1] == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).expect("read stdin");
        s
    } else {
        std::fs::read_to_string(&args[1]).unwrap_or_else(|e| {
            eprintln!("error: {}: {e}", args[1]);
            exit(2)
        })
    };

    let mut mapping = SparqlMapping::default();
    let mut limit = 0usize;
    let mut explain = false;
    let mut gql = false;
    let mut int_key = false;
    let mut key = String::from("iri");
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--import-ttl" => {
                let spec = match args.get(i + 1) {
                    Some(s) if !s.starts_with("--") => {
                        i += 1;
                        s.clone()
                    }
                    _ => "img:id".to_string(),
                };
                let (label, prop) = spec.split_once(':').unwrap_or_else(|| {
                    eprintln!("error: --import-ttl takes <label>:<prop>");
                    exit(2)
                });
                mapping = SparqlMapping::import_ttl(label, prop);
                key = prop.to_string();
                int_key = true;
            }
            "--local-names" => mapping.predicates = PredicateNaming::LocalName,
            "--node-label" => mapping.node_label = Some(take(&args, &mut i)),
            "--key" => key = take(&args, &mut i),
            "--int-key" => int_key = true,
            "--limit" => {
                limit = take(&args, &mut i).parse().unwrap_or_else(|_| {
                    eprintln!("error: --limit takes a number");
                    exit(2)
                })
            }
            "--explain" => explain = true,
            "--gql" => gql = true,
            other => {
                eprintln!("error: unknown option {other}");
                exit(2)
            }
        }
        i += 1;
    }
    mapping.node_key = if int_key {
        NodeKey::LocalNameInt { prop: key }
    } else {
        NodeKey::Iri { prop: key }
    };

    if explain && !gql {
        match translate(&text, &mapping) {
            Ok(q) => eprintln!("-- GQL: {q}"),
            Err(e) => {
                eprintln!("error: {e}");
                exit(1)
            }
        }
    }

    let t0 = Instant::now();
    let conn = Connection::open(db).unwrap_or_else(|e| {
        eprintln!("error: {db}: {e}");
        exit(1)
    });
    let opened = t0.elapsed();

    let t1 = Instant::now();
    let result = if gql {
        conn.execute_with_limit(&text, limit).and_then(|o| match o {
            frogql::Outcome::Rows(r) => Ok(r),
            _ => Err(frogql::Error::Execution("not a query".into())),
        })
    } else {
        conn.query_sparql(&text, &mapping, limit)
    };
    let elapsed = t1.elapsed();

    let rows = result.unwrap_or_else(|e| {
        eprintln!("error: {e}");
        exit(1)
    });
    println!("{}", rows.columns.join("\t"));
    for r in rows.iter() {
        let cells: Vec<String> = r.values().iter().map(|v| v.to_string()).collect();
        println!("{}", cells.join("\t"));
    }
    eprintln!(
        "{} rows, query {:.1} ms (open {:.1} ms)",
        rows.len(),
        elapsed.as_secs_f64() * 1e3,
        opened.as_secs_f64() * 1e3
    );
}

/// The value after the option at `*i`, advancing past it.
fn take(args: &[String], i: &mut usize) -> String {
    *i += 1;
    args.get(*i).cloned().unwrap_or_else(|| {
        eprintln!("error: {} needs a value", args[*i - 1]);
        exit(2)
    })
}
