//! Tests for the graph-type catalog (CREATE / USE / DROP GRAPH TYPE).
//! Coverage tracks the test plan in `docs/internals/graph-type-catalog-plan.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::catalog::GraphTypeCatalog;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::{Statement, TypeElement};
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::inference::infer_simple_schema;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::variable_type::{Schema, VariableType};

// -------- helpers --------

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_graph_type_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn build_schema_from_body(body: &[TypeElement]) -> Schema {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for el in body {
        match el {
            TypeElement::Node(vt) => nodes.push(vt.clone()),
            TypeElement::Edge(vt) => edges.push(vt.clone()),
        }
    }
    Schema::from_parts(nodes, edges)
}

fn parse_create(input: &str) -> (String, Vec<TypeElement>) {
    match parse_statement(input).expect("parse failed") {
        Statement::CreateGraphType { name, body } => (name, body),
        other => panic!("expected CreateGraphType, got {other:?}"),
    }
}

fn first_node_desc(body: &[TypeElement]) -> &DescriptorType {
    match &body[0] {
        TypeElement::Node(VariableType::Node(d)) => d,
        other => panic!("expected first element to be a Node, got {other:?}"),
    }
}

fn first_edge_desc(body: &[TypeElement]) -> (&DescriptorType, bool /* directed */) {
    match &body[0] {
        TypeElement::Edge(VariableType::EdgeDirectional { desc, .. }) => (desc, true),
        TypeElement::Edge(VariableType::EdgeNonDirectional { desc, .. }) => (desc, false),
        other => panic!("expected first element to be an Edge, got {other:?}"),
    }
}

fn props_map(pt: &PropertyType) -> BTreeMap<String, SimpleType> {
    match pt {
        PropertyType::Closed(m) | PropertyType::Open(m) => m.clone(),
        PropertyType::Zero => BTreeMap::new(),
    }
}

// =========================================================
// Parser
// =========================================================

#[test]
fn parse_create_simple_node() {
    let (name, body) = parse_create("CREATE GRAPH TYPE foo AS { (:Person {name STRING}) }");
    assert_eq!(name, "foo");
    assert_eq!(body.len(), 1);
    let d = first_node_desc(&body);
    assert!(matches!(&d.label, LabelType::Label(s) if s == "Person"));
    assert_eq!(props_map(&d.props).get("name"), Some(&SimpleType::S));
    assert!(matches!(&d.props, PropertyType::Closed(_)));
}

#[test]
fn parse_create_with_edge() {
    let (_name, body) =
        parse_create("CREATE GRAPH TYPE foo AS { (:A)-[:E {w FLOAT}]->(:B), (:A) }");
    assert_eq!(body.len(), 2);
    let (edge_desc, directed) = first_edge_desc(&body);
    assert!(directed);
    assert!(matches!(&edge_desc.label, LabelType::Label(s) if s == "E"));
    assert_eq!(props_map(&edge_desc.props).get("w"), Some(&SimpleType::F));
}

#[test]
fn parse_create_undirected_edge() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:A)~[:E]~(:B) }");
    let (_, directed) = first_edge_desc(&body);
    assert!(!directed);
}

#[test]
fn parse_create_left_directed_edge() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:A)<-[:E]-(:B) }");
    match &body[0] {
        TypeElement::Edge(VariableType::EdgeDirectional { left, right, .. }) => {
            // Normalized: src on the left. In `(:A)<-[:E]-(:B)`, B is the
            // source (arrow points left, so B → A).
            match left.as_ref() {
                VariableType::Node(d) => {
                    assert!(matches!(&d.label, LabelType::Label(s) if s == "B"))
                }
                _ => panic!("left should be Node"),
            }
            match right.as_ref() {
                VariableType::Node(d) => {
                    assert!(matches!(&d.label, LabelType::Label(s) if s == "A"))
                }
                _ => panic!("right should be Node"),
            }
        }
        _ => panic!("expected directional edge"),
    }
}

#[test]
fn parse_primitive_aliases() {
    let cases = [
        ("STRING", SimpleType::S),
        ("INT", SimpleType::Z),
        ("INTEGER", SimpleType::Z),
        ("FLOAT", SimpleType::F),
        ("BOOL", SimpleType::B),
        ("BOOLEAN", SimpleType::B),
        // Lowercase short forms also work for backward compat.
        ("int", SimpleType::Z),
        ("str", SimpleType::S),
        ("bool", SimpleType::B),
        ("float", SimpleType::F),
    ];
    for (alias, expected) in cases {
        let q = format!("CREATE GRAPH TYPE g AS {{ (:N {{p {alias}}}) }}");
        let (_, body) = parse_create(&q);
        let d = first_node_desc(&body);
        assert_eq!(
            props_map(&d.props).get("p"),
            Some(&expected),
            "alias {alias}"
        );
    }
}

#[test]
fn parse_rejects_create_default() {
    let err = parse_statement("CREATE GRAPH TYPE DEFAULT AS { (:X) }").unwrap_err();
    assert!(err.contains("DEFAULT"), "got: {err}");
}

#[test]
fn parse_rejects_drop_default() {
    let err = parse_statement("DROP GRAPH TYPE DEFAULT").unwrap_err();
    assert!(err.contains("DEFAULT"), "got: {err}");
}

#[test]
fn parse_use_graph_type() {
    match parse_statement("USE GRAPH TYPE foo").unwrap() {
        Statement::UseGraphType {
            name,
            refresh_default,
        } => {
            assert_eq!(name, "foo");
            assert!(!refresh_default);
        }
        _ => panic!("expected UseGraphType"),
    }
}

#[test]
fn parse_use_default_sets_refresh_flag() {
    match parse_statement("USE GRAPH TYPE DEFAULT").unwrap() {
        Statement::UseGraphType {
            name,
            refresh_default,
        } => {
            assert_eq!(name, "DEFAULT");
            assert!(refresh_default);
        }
        _ => panic!("expected UseGraphType"),
    }
}

#[test]
fn parse_drop_graph_type() {
    match parse_statement("DROP GRAPH TYPE foo").unwrap() {
        Statement::DropGraphType { name } => assert_eq!(name, "foo"),
        _ => panic!("expected DropGraphType"),
    }
}

#[test]
fn parse_trailing_semicolon_ok() {
    parse_statement("CREATE GRAPH TYPE foo AS { (:X) };").unwrap();
    parse_statement("USE GRAPH TYPE foo;").unwrap();
    parse_statement("DROP GRAPH TYPE foo;").unwrap();
    parse_statement("MATCH (x: Person) RETURN x.name;").unwrap();
}

// ---- composite types in property positions ----

#[test]
fn parse_list_via_keyword() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:Post {tags LIST<STRING>}) }");
    let d = first_node_desc(&body);
    assert_eq!(
        props_map(&d.props).get("tags"),
        Some(&SimpleType::List(Box::new(SimpleType::S)))
    );
}

#[test]
fn parse_list_via_bracket_form() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:Post {tags [STRING]}) }");
    let d = first_node_desc(&body);
    assert_eq!(
        props_map(&d.props).get("tags"),
        Some(&SimpleType::List(Box::new(SimpleType::S)))
    );
}

#[test]
fn parse_nested_list() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:M {matrix LIST<LIST<INT>>}) }");
    let d = first_node_desc(&body);
    assert_eq!(
        props_map(&d.props).get("matrix"),
        Some(&SimpleType::List(Box::new(SimpleType::List(Box::new(
            SimpleType::Z
        )))))
    );
}

#[test]
fn parse_nested_record_property_type() {
    let (_, body) =
        parse_create("CREATE GRAPH TYPE g AS { (:User {addr {city STRING, zip INT}}) }");
    let d = first_node_desc(&body);
    let addr = props_map(&d.props).remove("addr").expect("addr present");
    let mut expected = BTreeMap::new();
    expected.insert("city".to_string(), SimpleType::S);
    expected.insert("zip".to_string(), SimpleType::Z);
    assert_eq!(addr, SimpleType::Record(expected));
}

#[test]
fn parse_record_of_list_field() {
    let (_, body) =
        parse_create("CREATE GRAPH TYPE g AS { (:H {history {events LIST<STRING>, total INT}}) }");
    let d = first_node_desc(&body);
    let history = props_map(&d.props).remove("history").expect("history");
    let mut expected = BTreeMap::new();
    expected.insert(
        "events".to_string(),
        SimpleType::List(Box::new(SimpleType::S)),
    );
    expected.insert("total".to_string(), SimpleType::Z);
    assert_eq!(history, SimpleType::Record(expected));
}

#[test]
fn parse_list_of_records() {
    let (_, body) =
        parse_create("CREATE GRAPH TYPE g AS { (:L {logs LIST<{ts INT, msg STRING}>}) }");
    let d = first_node_desc(&body);
    let logs = props_map(&d.props).remove("logs").expect("logs");
    let mut rec = BTreeMap::new();
    rec.insert("ts".to_string(), SimpleType::Z);
    rec.insert("msg".to_string(), SimpleType::S);
    assert_eq!(logs, SimpleType::List(Box::new(SimpleType::Record(rec))));
}

#[test]
fn parse_union_in_property() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:I {id STRING | INT}) }");
    let d = first_node_desc(&body);
    let id = props_map(&d.props).remove("id").expect("id");
    // Either `Union(S, Z)` or `Union(Z, S)` — symmetric, but we assert
    // the parsed shape: left-most operand first.
    assert_eq!(
        id,
        SimpleType::Union(Box::new(SimpleType::S), Box::new(SimpleType::Z))
    );
}

#[test]
fn parse_wildcard_any_and_star() {
    for q in &[
        "CREATE GRAPH TYPE g AS { (:D {payload ANY}) }",
        "CREATE GRAPH TYPE g AS { (:D {payload *}) }",
    ] {
        let (_, body) = parse_create(q);
        let d = first_node_desc(&body);
        assert_eq!(props_map(&d.props).get("payload"), Some(&SimpleType::Star));
    }
}

#[test]
fn parse_deeply_mixed() {
    let (_, body) =
        parse_create("CREATE GRAPH TYPE g AS { (:Doc {doc {tags LIST<STRING | INT>, meta ANY}}) }");
    let d = first_node_desc(&body);
    let doc = props_map(&d.props).remove("doc").expect("doc");
    match doc {
        SimpleType::Record(fields) => {
            match fields.get("tags").unwrap() {
                SimpleType::List(inner) => match inner.as_ref() {
                    SimpleType::Union(_, _) => {}
                    other => panic!("expected union inside list, got {other:?}"),
                },
                other => panic!("expected list, got {other:?}"),
            }
            assert_eq!(fields.get("meta"), Some(&SimpleType::Star));
        }
        _ => panic!("expected record"),
    }
}

// ---- composite types in label positions ----

#[test]
fn parse_label_conjunction() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:Person&Employee {salary INT}) }");
    let d = first_node_desc(&body);
    match &d.label {
        LabelType::And(a, b) => {
            assert!(matches!(a.as_ref(), LabelType::Label(s) if s == "Person"));
            assert!(matches!(b.as_ref(), LabelType::Label(s) if s == "Employee"));
        }
        other => panic!("expected And label, got {other:?}"),
    }
}

#[test]
fn parse_label_disjunction() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:Customer|Vendor) }");
    let d = first_node_desc(&body);
    assert!(matches!(&d.label, LabelType::Or(_, _)));
}

#[test]
fn parse_label_negation() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:!Internal) }");
    let d = first_node_desc(&body);
    assert!(matches!(&d.label, LabelType::Neg(_)));
}

#[test]
fn parse_compound_label_in_edge() {
    let (_, body) = parse_create("CREATE GRAPH TYPE g AS { (:A)-[:E1&E2 {w FLOAT}]->(:B) }");
    let (edge_desc, _) = first_edge_desc(&body);
    assert!(matches!(&edge_desc.label, LabelType::And(_, _)));
}

// =========================================================
// Catalog state machine (no persistence)
// =========================================================

#[test]
fn catalog_register_and_set_active() {
    let mut c = GraphTypeCatalog::new();
    c.register("strict".into(), Schema::star()).unwrap();
    c.set_active("strict").unwrap();
    assert_eq!(c.active_name(), Some("strict"));
}

#[test]
fn catalog_install_default_then_swap() {
    let mut c = GraphTypeCatalog::new();
    c.install_default(Schema::star());
    c.register("strict".into(), Schema::star()).unwrap();
    c.set_active("strict").unwrap();
    assert_eq!(c.active_name(), Some("strict"));
    // Refreshing DEFAULT moves the active pointer back.
    c.install_default(Schema::star());
    assert_eq!(c.active_name(), Some("DEFAULT"));
}

// =========================================================
// End-to-end via LazyGraphStore (creates a real .gdb)
// =========================================================

fn save_minimal_graph(path: &std::path::Path) {
    let json = r#"{
        "nodes": [
            {"id": "ada", "labels": ["Person"], "props": {"name": "Ada"}},
            {"id": "post1", "labels": ["Post"], "props": {"title": "Hello", "tags": ["x", "y"]}}
        ],
        "edges": [
            {"id": "e1", "labels": ["WROTE"], "props": {}, "endpoints": ["ada", "post1"], "directionality": "->"}
        ]
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    g.save(path).unwrap();
}

#[test]
fn store_default_load_yields_empty_catalog_on_legacy_save() {
    let path = temp_db("legacy.gdb");
    save_minimal_graph(&path);
    let store = LazyGraphStore::open(&path).unwrap();
    // MemoryGraphStore::save doesn't populate DEFAULT — that's done by the import
    // pipeline in the gqlite binary. The catalog is empty here.
    assert!(store.catalog().active_name().is_none());
}

#[test]
fn store_install_default_persists() {
    let path = temp_db("default_persist.gdb");
    save_minimal_graph(&path);
    {
        let store = LazyGraphStore::open(&path).unwrap();
        let schema = infer_simple_schema(&store);
        store.catalog_mut().install_default(schema);
        store.save_catalog().unwrap();
    }
    // Reopen and verify
    let store = LazyGraphStore::open(&path).unwrap();
    let cat = store.catalog();
    assert_eq!(cat.active_name(), Some("DEFAULT"));
    assert!(cat.contains("DEFAULT"));
}

#[test]
fn store_register_strict_persists_and_typechecks() {
    let path = temp_db("strict_persist.gdb");
    save_minimal_graph(&path);

    {
        let store = LazyGraphStore::open(&path).unwrap();
        let body = match parse_statement("CREATE GRAPH TYPE strict AS { (:Person {name STRING}) }")
            .unwrap()
        {
            Statement::CreateGraphType { body, .. } => body,
            _ => panic!(),
        };
        let schema = build_schema_from_body(&body);
        store
            .catalog_mut()
            .register("strict".into(), schema)
            .unwrap();
        store.catalog_mut().set_active("strict").unwrap();
        store.save_catalog().unwrap();
    }

    let store = LazyGraphStore::open(&path).unwrap();
    assert_eq!(store.catalog().active_name(), Some("strict"));

    let active = store.catalog().active_schema();
    // Person is in the schema — compiles and (against the test data)
    // returns a row.
    let q = gqlrust::compile_query_with(&active, "MATCH (x: Person) RETURN x.name").unwrap();
    let rt = gqlrust::runtime::engine::Runtime::new(&store);
    match rt.run_query(&q, 10) {
        gqlrust::runtime::result::QueryResult::Projected(rows) => {
            assert_eq!(rows.len(), 1, "expected one Person row");
        }
        gqlrust::runtime::result::QueryResult::Raw(_) => panic!("expected projected result"),
    }

    // Foo is not in the strict schema. The typechecker refines the type
    // to bottom; the query still parses but yields no rows when run.
    let q_foo = gqlrust::compile_query_with(&active, "MATCH (x: Foo) RETURN x.name").unwrap();
    match rt.run_query(&q_foo, 10) {
        gqlrust::runtime::result::QueryResult::Projected(rows) => {
            assert!(rows.is_empty(), "Foo not in schema → expected zero rows");
        }
        gqlrust::runtime::result::QueryResult::Raw(_) => {}
    }
}

#[test]
fn store_drop_clears_active() {
    let path = temp_db("drop_clears.gdb");
    save_minimal_graph(&path);

    let store = LazyGraphStore::open(&path).unwrap();
    store
        .catalog_mut()
        .register("strict".into(), Schema::star())
        .unwrap();
    store.catalog_mut().set_active("strict").unwrap();
    store.save_catalog().unwrap();

    store.catalog_mut().drop("strict").unwrap();
    store.save_catalog().unwrap();
    assert!(store.catalog().active_name().is_none());
}

#[test]
fn store_drop_default_rejected() {
    let path = temp_db("drop_default.gdb");
    save_minimal_graph(&path);
    let store = LazyGraphStore::open(&path).unwrap();
    store.catalog_mut().install_default(Schema::star());
    let err = store.catalog_mut().drop("DEFAULT").unwrap_err();
    assert!(err.contains("DEFAULT"), "{err}");
}

// ---- Inference round-trip with composite values ----

#[test]
fn infer_then_typecheck_lists() {
    let path = temp_db("infer_lists.gdb");
    save_minimal_graph(&path);
    let store = LazyGraphStore::open(&path).unwrap();
    let schema = infer_simple_schema(&store);

    // Find the Post node descriptor and confirm `tags` came back as List<S>.
    let post = schema
        .nodes
        .iter()
        .find_map(|vt| match vt {
            VariableType::Node(d) if matches!(&d.label, LabelType::Label(s) if s == "Post") => {
                Some(d)
            }
            _ => None,
        })
        .expect("Post in schema");
    let tags = props_map(&post.props).remove("tags").expect("tags");
    assert_eq!(tags, SimpleType::List(Box::new(SimpleType::S)));
}

#[test]
fn schema_serialization_roundtrip_preserves_composites() {
    // Build a Schema covering each of the documented composite shapes
    // and confirm JSON roundtrip via `serde_json` preserves them.
    let mut nested_record = BTreeMap::new();
    nested_record.insert("city".to_string(), SimpleType::S);
    nested_record.insert("zip".to_string(), SimpleType::Z);

    let mut props = BTreeMap::new();
    props.insert("addr".to_string(), SimpleType::Record(nested_record));
    props.insert(
        "tags".to_string(),
        SimpleType::List(Box::new(SimpleType::S)),
    );
    props.insert(
        "id".to_string(),
        SimpleType::Union(Box::new(SimpleType::S), Box::new(SimpleType::Z)),
    );
    props.insert("payload".to_string(), SimpleType::Star);

    let labels = LabelType::And(
        Box::new(LabelType::Label("Person".into())),
        Box::new(LabelType::Label("Employee".into())),
    );

    let node = VariableType::Node(DescriptorType::new(labels, PropertyType::Closed(props)));
    let schema = Schema::from_parts(vec![node], vec![]);

    let json = serde_json::to_string(&schema).unwrap();
    let back: Schema = serde_json::from_str(&json).unwrap();

    // Equality on Schema isn't derived (it has fields with non-Eq inner
    // types in some places), so we compare via JSON re-encoding.
    let re = serde_json::to_string(&back).unwrap();
    assert_eq!(json, re);
}

// ---- Backward compat: opening a legacy .gdb with catalog_root=0 ----

#[test]
fn legacy_gdb_opens_with_empty_catalog() {
    let path = temp_db("legacy_zero_root.gdb");
    save_minimal_graph(&path);
    // Open and confirm we can compile + run a query against the empty
    // catalog (Schema::star) without errors.
    let store = LazyGraphStore::open(&path).unwrap();
    let active = store.catalog().active_schema();
    let q = gqlrust::compile_query_with(&active, "MATCH (x: Person) RETURN x.name").unwrap();
    let rt = gqlrust::runtime::engine::Runtime::new(&store);
    let _ = rt.run_query(&q, 10);
}

// =========================================================
// SHOW / VALIDATE
// =========================================================

#[test]
fn parse_show_graph_types() {
    match parse_statement("SHOW GRAPH TYPES").unwrap() {
        Statement::ShowGraphTypes => {}
        other => panic!("expected ShowGraphTypes, got {other:?}"),
    }
}

#[test]
fn parse_show_graph_type_named() {
    match parse_statement("SHOW GRAPH TYPE foo").unwrap() {
        Statement::ShowGraphType { name } => assert_eq!(name, "foo"),
        other => panic!("expected ShowGraphType, got {other:?}"),
    }
}

#[test]
fn parse_show_graph_type_default() {
    // SHOW accepts the DEFAULT keyword and normalizes the case.
    match parse_statement("SHOW GRAPH TYPE DEFAULT").unwrap() {
        Statement::ShowGraphType { name } => assert_eq!(name, "DEFAULT"),
        other => panic!("expected ShowGraphType, got {other:?}"),
    }
}

#[test]
fn parse_show_current_graph_type() {
    match parse_statement("SHOW CURRENT GRAPH TYPE").unwrap() {
        Statement::ShowCurrentGraphType => {}
        other => panic!("expected ShowCurrentGraphType, got {other:?}"),
    }
}

#[test]
fn parse_validate_graph_type() {
    match parse_statement("VALIDATE GRAPH TYPE foo").unwrap() {
        Statement::ValidateGraphType { name } => assert_eq!(name, "foo"),
        other => panic!("expected ValidateGraphType, got {other:?}"),
    }
}

#[test]
fn validate_passes_against_inferred_default() {
    let path = temp_db("validate_default.gdb");
    save_minimal_graph(&path);
    let store = LazyGraphStore::open(&path).unwrap();
    let schema = infer_simple_schema(&store);
    store.catalog_mut().install_default(schema.clone());

    let report = gqlrust::typing::validate::validate_against_data(&store, &schema);
    assert!(report.ok(), "DEFAULT should match its own inferred shape");
    assert_eq!(report.nodes_checked, 2);
    assert_eq!(report.edges_checked, 1);
}

#[test]
fn validate_flags_strict_mismatch() {
    let path = temp_db("validate_strict_mismatch.gdb");
    save_minimal_graph(&path);
    let store = LazyGraphStore::open(&path).unwrap();

    // strict requires Person nodes to have a `name STRING` and an `age INT`.
    // The fixture has a Person with name only, so validation should flag it.
    let body =
        match parse_statement("CREATE GRAPH TYPE strict AS { (:Person {name STRING, age INT}) }")
            .unwrap()
        {
            Statement::CreateGraphType { body, .. } => body,
            _ => panic!(),
        };
    let schema = build_schema_from_body(&body);
    let report = gqlrust::typing::validate::validate_against_data(&store, &schema);
    assert!(!report.ok());
    assert!(report.node_violations >= 1);
}

#[test]
fn validation_status_persists() {
    let path = temp_db("validation_persists.gdb");
    save_minimal_graph(&path);
    {
        let store = LazyGraphStore::open(&path).unwrap();
        let body = match parse_statement("CREATE GRAPH TYPE strict AS { (:Person {name STRING}) }")
            .unwrap()
        {
            Statement::CreateGraphType { body, .. } => body,
            _ => panic!(),
        };
        let schema = build_schema_from_body(&body);
        store
            .catalog_mut()
            .register("strict".into(), schema.clone())
            .unwrap();
        let report = gqlrust::typing::validate::validate_against_data(&store, &schema);
        store.catalog_mut().record_validation(
            "strict",
            gqlrust::runtime::catalog::ValidationStatus {
                against_node_count: report.nodes_checked,
                against_edge_count: report.edges_checked,
                violations: report.total_violations(),
                validated_at_unix: 12345,
            },
        );
        store.save_catalog().unwrap();
    }
    let store = LazyGraphStore::open(&path).unwrap();
    let v = store
        .catalog()
        .validation_for("strict")
        .cloned()
        .expect("validation cached");
    assert_eq!(v.against_node_count, 2);
    assert_eq!(v.validated_at_unix, 12345);
}

#[test]
fn re_register_clears_validation_cache() {
    let mut cat = GraphTypeCatalog::new();
    cat.register("strict".into(), Schema::star()).unwrap();
    cat.record_validation(
        "strict",
        gqlrust::runtime::catalog::ValidationStatus {
            against_node_count: 100,
            against_edge_count: 200,
            violations: 0,
            validated_at_unix: 1,
        },
    );
    // CREATE GRAPH TYPE strict (replacement) drops the old verdict.
    cat.register("strict".into(), Schema::star()).unwrap();
    assert!(cat.validation_for("strict").is_none());
}

#[test]
fn drop_clears_validation_cache() {
    let mut cat = GraphTypeCatalog::new();
    cat.register("strict".into(), Schema::star()).unwrap();
    cat.record_validation(
        "strict",
        gqlrust::runtime::catalog::ValidationStatus {
            against_node_count: 100,
            against_edge_count: 200,
            violations: 0,
            validated_at_unix: 1,
        },
    );
    cat.drop("strict").unwrap();
    assert!(cat.validation_for("strict").is_none());
}

#[test]
fn install_default_clears_default_validation() {
    let mut cat = GraphTypeCatalog::new();
    cat.install_default(Schema::star());
    cat.record_validation(
        "DEFAULT",
        gqlrust::runtime::catalog::ValidationStatus {
            against_node_count: 100,
            against_edge_count: 200,
            violations: 0,
            validated_at_unix: 1,
        },
    );
    cat.install_default(Schema::star()); // refresh
    assert!(cat.validation_for("DEFAULT").is_none());
}

#[test]
fn format_schema_round_trips_through_parser() {
    // Build a schema, format it via SHOW, prepend CREATE GRAPH TYPE, and
    // confirm the result re-parses to a structurally similar schema.
    // We don't expect bit-equality (Open vs Closed nuances may differ),
    // but the labels and prop type names should round-trip.
    let body = match parse_statement(
        "CREATE GRAPH TYPE g AS { (:Person {name STRING, age INT}), (:A)-[:E]->(:B) }",
    )
    .unwrap()
    {
        Statement::CreateGraphType { body, .. } => body,
        _ => panic!(),
    };
    let schema = build_schema_from_body(&body);
    let out = gqlrust::typing::format::format_schema(&schema);
    assert!(out.contains("(:Person"));
    assert!(out.contains("name STRING"));
    assert!(out.contains("age INT"));
    assert!(out.contains("(:A)-[:E]->(:B)"));
}

// =========================================================
// Refine-to-Zero diagnostics: warnings when patterns don't match.
// =========================================================

fn warnings_for(input: &str, schema: &Schema) -> Vec<String> {
    gqlrust::compile_query_with_diagnostics_with(schema, input)
        .map(|r| r.warnings)
        .unwrap_or_default()
}

fn schema_with_genre_and_actor() -> Schema {
    let body = match parse_statement(
        "CREATE GRAPH TYPE g AS { (:Genre {name STRING}), (:Actor {name STRING}) }",
    )
    .unwrap()
    {
        Statement::CreateGraphType { body, .. } => body,
        _ => panic!(),
    };
    build_schema_from_body(&body)
}

#[test]
fn warning_label_combination_not_in_schema() {
    let schema = schema_with_genre_and_actor();
    let ws = warnings_for("(:Actor&Genre)", &schema);
    assert!(
        ws.iter().any(|w| w.contains("label not in schema")),
        "expected label-not-in-schema warning, got {ws:?}"
    );
}

#[test]
fn warning_unknown_label() {
    let schema = schema_with_genre_and_actor();
    let ws = warnings_for("(:Foo)", &schema);
    assert!(
        ws.iter()
            .any(|w| w.contains("Foo") && w.contains("label not in schema")),
        "expected Foo label warning, got {ws:?}"
    );
}

#[test]
fn warning_property_type_mismatch_distinguishes_from_label() {
    let schema = schema_with_genre_and_actor();
    let ws = warnings_for("(:Genre {name bool})", &schema);
    assert!(
        ws.iter().any(|w| w.contains("properties differ")),
        "expected properties-differ warning, got {ws:?}"
    );
    assert!(
        !ws.iter().any(|w| w.contains("label not in schema")),
        "should not blame the label, got {ws:?}"
    );
}

#[test]
fn warning_silent_under_permissive_schema() {
    // No active type → permissive Schema::star(); refine never returns Zero,
    // so even bizarre label combos compile without these warnings.
    let star = Schema::star();
    let ws = warnings_for("(:Actor&Genre)", &star);
    assert!(
        !ws.iter()
            .any(|w| w.contains("label not in schema") || w.contains("properties differ")),
        "permissive mode should not produce refine warnings, got {ws:?}"
    );
}

#[test]
fn warning_edge_label_not_in_schema() {
    let body = match parse_statement("CREATE GRAPH TYPE g AS { (:A)-[:KNOWS]->(:B) }").unwrap() {
        Statement::CreateGraphType { body, .. } => body,
        _ => panic!(),
    };
    let schema = build_schema_from_body(&body);
    let ws = warnings_for("(:A)-[:LOVES]->(:B)", &schema);
    assert!(
        ws.iter()
            .any(|w| w.contains("LOVES") && w.contains("label not in schema")),
        "expected LOVES edge-label warning, got {ws:?}"
    );
}

#[test]
fn warning_collapsed_variable_binding() {
    let schema = schema_with_genre_and_actor();
    // Each side refines fine on its own; the meet over `x` collapses
    // because no node is both Actor and Genre.
    let ws = warnings_for("(x:Actor)(x:Genre)", &schema);
    assert!(
        ws.iter()
            .any(|w| w.contains("variable x") && w.contains("under the active schema")),
        "expected collapsed-binding warning for x, got {ws:?}"
    );
    // Order shouldn't matter.
    let ws_rev = warnings_for("(x:Genre)(x:Actor)", &schema);
    assert!(
        ws_rev.iter().any(|w| w.contains("variable x")),
        "expected collapsed-binding warning regardless of order, got {ws_rev:?}"
    );
}

#[test]
fn warning_compatible_variable_no_collapse_warning() {
    let schema = schema_with_genre_and_actor();
    // (x:Actor)(x:Actor) — same type both sides, meet is non-empty,
    // should not produce a collapsed-binding warning.
    let ws = warnings_for("(x:Actor)(x:Actor)", &schema);
    assert!(
        !ws.iter().any(|w| w.contains("cannot be both")),
        "no collapse warning expected, got {ws:?}"
    );
}
