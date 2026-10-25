//! Every integration test, as one binary.
//!
//! Each file in `tests/` used to be a crate of its own, linked separately
//! against the library: 97 links, and on macOS a first-execution cost of
//! several seconds per freshly linked binary, paid once per file. One crate
//! links once and pays it once. See
//! <https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html>.
//!
//! What forced the split was the environment. A test setting a `FROGQL_*`
//! variable changed the code path of every test sharing its process. The
//! kill switches are values now (`RuntimeCfg`, `OptimizerCfg`,
//! `VectorStore::disable`), so a test sets the one it means and touches
//! nothing else. Do not bring `std::env::set_var` back into this crate: a
//! test that needs the environment itself runs as a child process, the way
//! `overlay_index_test` does.
//!
//! `bench_test` stays a target of its own: it installs a
//! `#[global_allocator]` to measure memory, and a binary has only one.
//!
//! One file's tests: `cargo test --test it runtime_test::`.

mod aggregates_proptest;
mod anydir_iso_test;
mod anydir_path_consistency_test;
mod auto_index_memory_test;
mod builtin_floor_cast_test;
mod catalog_persistence_test;
mod cli_args_test;
mod coalesce_test;
mod collect_list_test;
mod compact_ltj_test;
mod compile_diagnostics;
mod connection_test;
mod correlated_nearest_test;
mod correlated_subquery_test;
mod count_test;
mod cross_clause_scope_test;
mod disconnected_join_test;
mod division_test;
mod dm_default_test;
mod dm_delete_expr_test;
mod dm_label_test;
mod dm_match_optimized_test;
mod dm_persistence_test;
mod dm_remove_test;
mod dm_rollback_test;
mod dm_runtime_test;
mod dm_schema_test;
mod dm_set_test;
mod dm_shares_triple_index_test;
mod dump_test;
mod edge_direction_ltj_test;
mod edge_label_algebra_test;
mod elaborate_test;
mod exists_fold_test;
mod exists_runtime_test;
mod first_class_node_edge_values_test;
mod float_index_test;
mod float_test;
mod graph_type_test;
mod group_by_alias_test;
mod group_by_var_test;
mod group_variable_test;
mod ic7_test;
mod iso_expr_test;
mod iso_multiplicity_test;
mod lattice_proptest;
mod lazy_mut_test;
mod lean_alignment_test;
mod list_comprehension_test;
mod list_test;
mod ltj_all_pinned_test;
mod ltj_delta_test;
mod ltj_label_disjunction_test;
mod ltj_persist_test;
mod ltj_reverse_path_test;
mod match_pushdown_test;
mod math_builtin_test;
mod memory_mut_test;
mod multi_match_proptest;
mod multi_match_test;
mod named_path_pushdown_test;
mod named_path_test;
mod nearest_parse_test;
mod nearest_runtime_test;
mod null_test;
mod null_type_test;
mod open_bytes_test;
mod optional_match_test;
mod order_by_optimization_test;
mod order_by_test;
mod ordering_test;
mod overlay_index_test;
mod parallel_edge_test;
mod parse_and_run_test;
mod parser_dm_test;
mod parser_test;
mod path_prefix_test;
mod pushdown_null_test;
mod query_budget_test;
mod record_expr_test;
mod record_test;
mod return_item_shape_test;
mod runtime_test;
mod seeded_repetition_test;
mod shortest_bfs_test;
mod store_runtime_test;
mod temporal_test;
mod text2gql_test;
mod three_valued_logic_test;
mod typecheck_gaps_order_by_test;
mod typecheck_smoke;
mod typecheck_test;
mod value_subquery_pushdown_test;
mod value_subquery_test;
mod vector_kill_switch_test;
mod vector_sidecar_test;
mod vector_strategy_equiv_test;
mod veo_adaptive_test;
mod veo_point_predicate_test;

/// A file in `tests/it/` without its `mod` line compiles into nothing and
/// runs nothing, and nobody notices: the CI's hand-kept `--test` list once
/// skipped 42 of 57 files that way. A file left directly in `tests/` does
/// run, as a binary of its own, which is the cost this crate exists to
/// remove. Both fail here instead.
#[test]
fn every_test_file_is_a_module_of_this_crate() {
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let main = include_str!("main.rs");
    let stems = |dir: &std::path::Path| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.unwrap().file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".rs").map(str::to_string))
            .collect();
        v.sort();
        v
    };
    for stem in stems(&tests.join("it")) {
        assert!(
            stem == "main" || main.contains(&format!("\nmod {stem};\n")),
            "tests/it/{stem}.rs has no `mod {stem};` in tests/it/main.rs, so it never runs"
        );
    }
    for stem in stems(&tests) {
        assert!(
            stem == "bench_test",
            "tests/{stem}.rs is a binary of its own; move it into tests/it/ with a `mod` line"
        );
    }
}
