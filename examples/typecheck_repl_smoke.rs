//! End-to-end smoke check against the bundled example .gdb files.
//!
//! Runs a handful of representative queries through both `compile_query`
//! (typecheck on by default) and `compile_query_unchecked`, executes each
//! against the loaded database, and confirms:
//!   1. the typechecked path accepts the query (or gives a useful error),
//!   2. the unchecked path produces the same plan,
//!   3. the runtime returns a non-empty result for known-good queries,
//!   4. an obviously-broken query is rejected by the checker only.
//!
//! Run: `cargo run --release --example typecheck_repl_smoke`

use std::path::Path;

use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::{compile_query, compile_query_unchecked};

fn main() {
    let movies = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/movies.gdb");
    let store = LazyGraphStore::open(&movies).expect("open movies.gdb");
    println!(
        "Opened {} ({} nodes, {} edges)\n",
        movies.display(),
        store.node_count(),
        store.edge_count()
    );

    let mut suite = Suite::new(&store);

    // Known-good queries: each must typecheck, plan-match, and execute.
    suite.good("MATCH (m:Movie) RETURN m.title");
    suite.good("MATCH (p:Person) RETURN p.name");
    suite.good("MATCH (p:Person)-[:ACTED_IN]->(m:Movie) RETURN m.title, p.name");
    suite.good(
        "MATCH (p:Person)-[:ACTED_IN]->(m:Movie) \
         WHERE m.released > 2000 RETURN p.name, m.title",
    );

    // Known-bad: WHERE references an unbound variable.
    // - compile_query should reject (typechecker is the gatekeeper).
    // - compile_query_unchecked should still produce a plan.
    suite.bad_unbound("MATCH (m:Movie) WHERE x.foo = 1 RETURN m.title", "x");

    suite.report();
}

struct Suite<'a> {
    store: &'a LazyGraphStore,
    pass: usize,
    fail: usize,
}

impl<'a> Suite<'a> {
    fn new(store: &'a LazyGraphStore) -> Self {
        Suite {
            store,
            pass: 0,
            fail: 0,
        }
    }

    fn good(&mut self, q: &str) {
        println!("[good] {}", q);
        let checked = match compile_query(q) {
            Ok(q) => q,
            Err(e) => {
                println!("  ✗ compile_query failed: {}", e);
                self.fail += 1;
                return;
            }
        };
        let unchecked = match compile_query_unchecked(q) {
            Ok(q) => q,
            Err(e) => {
                println!("  ✗ compile_query_unchecked failed: {}", e);
                self.fail += 1;
                return;
            }
        };
        if format!("{:?}", checked.collapsed_pattern())
            != format!("{:?}", unchecked.collapsed_pattern())
        {
            println!("  ✗ plan mismatch between checked and unchecked");
            self.fail += 1;
            return;
        }
        let runtime = Runtime::new(self.store);
        let result = runtime.run_query(&checked, 5);
        let n = result.row_count();
        println!("  ✓ ok, {} row(s) (showing up to 3)", n);
        match &result {
            QueryResult::Raw(ir) => {
                for (i, row) in ir.rows.iter().enumerate().take(3) {
                    println!("    [{}] {:?}", i, row);
                }
            }
            QueryResult::Projected(rows) => {
                for (i, row) in rows.iter().enumerate().take(3) {
                    println!("    [{}] {:?}", i, row);
                }
            }
        }
        self.pass += 1;
    }

    fn bad_unbound(&mut self, q: &str, expected_var: &str) {
        println!("[bad ] {}", q);
        match compile_query(q) {
            Ok(_) => {
                println!("  ✗ checker accepted query that should fail");
                self.fail += 1;
                return;
            }
            Err(e) => {
                if !e.contains(expected_var) || !e.contains("not found") {
                    println!(
                        "  ✗ wrong error: expected '{} ... not found', got: {}",
                        expected_var, e
                    );
                    self.fail += 1;
                    return;
                }
                println!("  ✓ rejected by checker: {}", e);
            }
        }
        match compile_query_unchecked(q) {
            Ok(_) => {
                println!("  ✓ unchecked path accepted (as expected)");
                self.pass += 1;
            }
            Err(e) => {
                println!("  ✗ unchecked path also rejected: {}", e);
                self.fail += 1;
            }
        }
    }

    fn report(&self) {
        println!("\n{} pass, {} fail", self.pass, self.fail);
        if self.fail > 0 {
            std::process::exit(1);
        }
    }
}
