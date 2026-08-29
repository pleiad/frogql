//! Differential suite for the typechecker's schema label index
//! (`src/typing/schema_index.rs`): for arbitrary schemas and query
//! descriptors — including adversarial label trees with `Star`/`Top`/
//! `Empty`/`Neg`/`And`/`Or` on both sides — `refine` must produce
//! bit-identical results with the index enabled (default) and disabled
//! (`GQLITE_DISABLE_TC_SCHEMA_INDEX=1`).
//!
//! Kept as a single proptest target because the kill switch is a
//! process-global env var and cargo runs test *functions* in threads
//! (cases within one proptest run sequentially). Each side uses its own
//! freshly built `Schema` so the refine memo of one setting cannot mask
//! the other.

use proptest::prelude::*;
use std::collections::BTreeMap;

use frogql::typing::descriptor_type::DescriptorType;
use frogql::typing::label_type::LabelType;
use frogql::typing::property_type::PropertyType;
use frogql::typing::simple_type::SimpleType;
use frogql::typing::variable_type::{Schema, VariableType};

fn arb_label_tree() -> impl Strategy<Value = LabelType> {
    let leaf = prop_oneof![
        3 => prop_oneof![
            Just(LabelType::Label("A".into())),
            Just(LabelType::Label("B".into())),
            Just(LabelType::Label("C".into())),
            Just(LabelType::Label("D".into())),
        ],
        1 => Just(LabelType::Star),
        1 => Just(LabelType::Top),
        1 => Just(LabelType::Empty),
    ];
    leaf.prop_recursive(3, 12, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| LabelType::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| LabelType::Or(Box::new(a), Box::new(b))),
            inner.prop_map(|a| LabelType::Neg(Box::new(a))),
        ]
    })
}

fn arb_props() -> impl Strategy<Value = PropertyType> {
    let kv = prop_oneof![
        Just(("k1".to_string(), SimpleType::Z)),
        Just(("k2".to_string(), SimpleType::S)),
    ];
    proptest::collection::vec(kv, 0..2)
        .prop_map(|kvs| PropertyType::Open(kvs.into_iter().collect::<BTreeMap<_, _>>()))
}

fn arb_desc() -> impl Strategy<Value = DescriptorType> {
    (arb_label_tree(), arb_props()).prop_map(|(l, p)| DescriptorType::new(l, p))
}

fn arb_node_entry() -> impl Strategy<Value = VariableType> {
    arb_desc().prop_map(VariableType::Node)
}

fn arb_edge_entry() -> impl Strategy<Value = VariableType> {
    (arb_desc(), arb_desc(), arb_desc(), any::<bool>()).prop_map(|(d, l, r, directed)| {
        let (left, right) = (
            Box::new(VariableType::Node(l)),
            Box::new(VariableType::Node(r)),
        );
        if directed {
            VariableType::EdgeDirectional {
                desc: d,
                left,
                right,
            }
        } else {
            VariableType::EdgeNonDirectional {
                desc: d,
                left,
                right,
            }
        }
    })
}

fn arb_query() -> impl Strategy<Value = VariableType> {
    prop_oneof![
        arb_desc().prop_map(VariableType::Node),
        arb_desc().prop_map(VariableType::edge_directional),
        arb_desc().prop_map(VariableType::edge_non_directional),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn indexed_refine_equals_linear_refine(
        nodes in proptest::collection::vec(arb_node_entry(), 1..6),
        edges in proptest::collection::vec(arb_edge_entry(), 1..6),
        query in arb_query(),
    ) {
        // Linear scan (index disabled), on its own schema instance.
        std::env::set_var("GQLITE_DISABLE_TC_SCHEMA_INDEX", "1");
        let schema_linear = Schema::from_parts(nodes.clone(), edges.clone());
        let linear = VariableType::refine(&schema_linear, &query);
        std::env::remove_var("GQLITE_DISABLE_TC_SCHEMA_INDEX");

        // Indexed (default), fresh schema so no memo carries over.
        let schema_indexed = Schema::from_parts(nodes, edges);
        let indexed = VariableType::refine(&schema_indexed, &query);

        prop_assert_eq!(
            linear, indexed,
            "indexed refine diverged from linear scan for query {:?}", query
        );
    }
}
