//! CREATE-compatible pretty-printer for `Schema` values.
//!
//! The existing `Display` impls on `LabelType` / `PropertyType` /
//! `SimpleType` use research-paper notation (`⊥`, `⊤`, `*` for the open
//! marker). The catalog DDL, on the other hand, uses ISO uppercase
//! aliases (`STRING`, `INT`, `LIST<T>`) and a structural shape that can
//! be fed back into `parse_statement`. This module keeps the two sets
//! of conventions cleanly separated; `Display` stays for diagnostics,
//! these helpers for `SHOW GRAPH TYPE`.

use std::collections::BTreeMap;

use super::descriptor_type::DescriptorType;
use super::label_type::LabelType;
use super::property_type::PropertyType;
use super::simple_type::SimpleType;
use super::variable_type::{Schema, VariableType};

/// Render `schema` as a CREATE GRAPH TYPE body. Returns a multi-line
/// string with one element per line, indented for readability.
pub fn format_schema(schema: &Schema) -> String {
    let mut out = String::new();
    if !schema.nodes.is_empty() {
        out.push_str("Node types:\n");
        for vt in schema.nodes.iter() {
            out.push_str("    ");
            out.push_str(&format_variable(vt));
            out.push('\n');
        }
    }
    if !schema.edges.is_empty() {
        if !schema.nodes.is_empty() {
            out.push('\n');
        }
        out.push_str("Edge types:\n");
        for vt in schema.edges.iter() {
            out.push_str("    ");
            out.push_str(&format_variable(vt));
            out.push('\n');
        }
    }
    if schema.nodes.is_empty() && schema.edges.is_empty() {
        out.push_str("(empty schema)\n");
    }
    out
}

/// Single-line CREATE-style rendering of one `VariableType`.
pub fn format_variable(vt: &VariableType) -> String {
    match vt {
        VariableType::Node(d) => format_node_descriptor(d),
        VariableType::EdgeDirectional { desc, left, right } => format!(
            "{}-[{}]->{}",
            format_endpoint(left),
            format_edge_descriptor(desc),
            format_endpoint(right),
        ),
        VariableType::EdgeNonDirectional { desc, left, right } => format!(
            "{}~[{}]~{}",
            format_endpoint(left),
            format_edge_descriptor(desc),
            format_endpoint(right),
        ),
        // Unions / Group / Zero appear in inference output too, even
        // though CREATE syntax doesn't accept them at the top level.
        // Render best-effort so SHOW never blanks out.
        VariableType::Union(a, b) => format!("({}) | ({})", format_variable(a), format_variable(b)),
        VariableType::Group(t) => format!("group<{}>", format_variable(t)),
        VariableType::Null => "Null".to_string(),
        VariableType::Path => "PATH".to_string(),
        VariableType::Zero => "⊥".to_string(),
    }
}

fn format_endpoint(vt: &VariableType) -> String {
    match vt {
        VariableType::Node(d) => format_node_descriptor(d),
        // Only Node is valid here per the schema-body grammar; render
        // a fallback rather than panicking.
        _ => format!("({})", format_variable(vt)),
    }
}

fn format_node_descriptor(d: &DescriptorType) -> String {
    let label = format_label(&d.label);
    let props = format_property_record(&d.props);
    match (label.is_empty(), props.is_empty()) {
        (true, true) => "()".to_string(),
        (false, true) => format!("(:{label})"),
        (true, false) => format!("({props})"),
        (false, false) => format!("(:{label} {props})"),
    }
}

fn format_edge_descriptor(d: &DescriptorType) -> String {
    let label = format_label(&d.label);
    let props = format_property_record(&d.props);
    match (label.is_empty(), props.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(":{label}"),
        (true, false) => props,
        (false, false) => format!(":{label} {props}"),
    }
}

/// Returns the bare label expression (no leading colon). Empty string
/// when the label is unconstrained (`Star` / `Top`).
fn format_label(lt: &LabelType) -> String {
    match lt {
        LabelType::Label(s) => s.clone(),
        LabelType::Star | LabelType::Top => String::new(),
        LabelType::Empty => "ε".to_string(),
        LabelType::And(a, b) => {
            let l = format_label_inner(a);
            let r = format_label_inner(b);
            format!("{l}&{r}")
        }
        LabelType::Or(a, b) => {
            let l = format_label_inner(a);
            let r = format_label_inner(b);
            format!("{l}|{r}")
        }
        LabelType::Neg(inner) => {
            let s = format_label_inner(inner);
            format!("!{s}")
        }
    }
}

fn format_label_inner(lt: &LabelType) -> String {
    match lt {
        LabelType::And(_, _) | LabelType::Or(_, _) => format!("({})", format_label(lt)),
        _ => format_label(lt),
    }
}

fn format_property_record(pt: &PropertyType) -> String {
    match pt {
        PropertyType::Open(m) if m.is_empty() => String::new(),
        PropertyType::Closed(m) if m.is_empty() => "{}".to_string(),
        PropertyType::Open(m) => format!("{{{}, *}}", format_field_map(m)),
        PropertyType::Closed(m) => format!("{{{}}}", format_field_map(m)),
        PropertyType::Zero => "⊥".to_string(),
    }
}

fn format_field_map(m: &BTreeMap<String, SimpleType>) -> String {
    m.iter()
        .map(|(k, v)| format!("{k} {}", format_simple_type(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render a `SimpleType` in the catalog DDL style: uppercase atoms,
/// `LIST<T>` for lists, `T1 | T2` for unions, `{...}` for records.
fn format_simple_type(t: &SimpleType) -> String {
    match t {
        SimpleType::Z => "INT".to_string(),
        SimpleType::F => "FLOAT".to_string(),
        SimpleType::B => "BOOL".to_string(),
        SimpleType::S => "STRING".to_string(),
        SimpleType::Star => "ANY".to_string(),
        SimpleType::Zero => "⊥".to_string(),
        SimpleType::Union(a, b) => format!(
            "{} | {}",
            format_simple_type_atom(a),
            format_simple_type_atom(b)
        ),
        SimpleType::List(inner) => format!("LIST<{}>", format_simple_type(inner)),
        SimpleType::Group(inner) => format!("group<{}>", format_simple_type(inner)),
        SimpleType::Record(fields) => format!("{{{}}}", format_field_map(fields)),
        SimpleType::Node => "NODE".to_string(),
        SimpleType::Edge => "EDGE".to_string(),
        SimpleType::Path => "PATH".to_string(),
    }
}

fn format_simple_type_atom(t: &SimpleType) -> String {
    match t {
        SimpleType::Union(_, _) => format!("({})", format_simple_type(t)),
        _ => format_simple_type(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(fields: &[(&str, SimpleType)]) -> PropertyType {
        let mut m = BTreeMap::new();
        for (k, v) in fields {
            m.insert((*k).to_string(), v.clone());
        }
        PropertyType::Closed(m)
    }

    #[test]
    fn formats_simple_node() {
        let d = DescriptorType::new(
            LabelType::Label("Person".into()),
            closed(&[("name", SimpleType::S), ("age", SimpleType::Z)]),
        );
        let s = format_node_descriptor(&d);
        // BTreeMap orders by key, so age < name.
        assert_eq!(s, "(:Person {age INT, name STRING})");
    }

    #[test]
    fn formats_directed_edge() {
        let edge = VariableType::EdgeDirectional {
            desc: DescriptorType::new(
                LabelType::Label("KNOWS".into()),
                closed(&[("since", SimpleType::Z)]),
            ),
            left: Box::new(VariableType::Node(DescriptorType::new(
                LabelType::Label("Person".into()),
                PropertyType::open_empty(),
            ))),
            right: Box::new(VariableType::Node(DescriptorType::new(
                LabelType::Label("Person".into()),
                PropertyType::open_empty(),
            ))),
        };
        assert_eq!(
            format_variable(&edge),
            "(:Person)-[:KNOWS {since INT}]->(:Person)"
        );
    }

    #[test]
    fn formats_undirected_edge() {
        let edge = VariableType::EdgeNonDirectional {
            desc: DescriptorType::new(
                LabelType::Label("FRIENDS".into()),
                PropertyType::open_empty(),
            ),
            left: Box::new(VariableType::Node(DescriptorType::new(
                LabelType::Label("Person".into()),
                PropertyType::open_empty(),
            ))),
            right: Box::new(VariableType::Node(DescriptorType::new(
                LabelType::Label("Person".into()),
                PropertyType::open_empty(),
            ))),
        };
        assert_eq!(format_variable(&edge), "(:Person)~[:FRIENDS]~(:Person)");
    }

    #[test]
    fn formats_compound_label() {
        let d = DescriptorType::new(
            LabelType::And(
                Box::new(LabelType::Label("Person".into())),
                Box::new(LabelType::Label("Employee".into())),
            ),
            PropertyType::open_empty(),
        );
        assert_eq!(format_node_descriptor(&d), "(:Person&Employee)");
    }

    #[test]
    fn formats_list_record_union_any() {
        let mut nested = BTreeMap::new();
        nested.insert("city".to_string(), SimpleType::S);
        let d = DescriptorType::new(
            LabelType::Label("Doc".into()),
            closed(&[
                ("tags", SimpleType::List(Box::new(SimpleType::S))),
                (
                    "id",
                    SimpleType::Union(Box::new(SimpleType::S), Box::new(SimpleType::Z)),
                ),
                ("payload", SimpleType::Star),
                ("addr", SimpleType::Record(nested)),
            ]),
        );
        let s = format_node_descriptor(&d);
        assert!(s.contains("tags LIST<STRING>"));
        assert!(s.contains("id STRING | INT"));
        assert!(s.contains("payload ANY"));
        assert!(s.contains("addr {city STRING}"));
    }

    #[test]
    fn formats_open_record_with_star_marker() {
        let d = DescriptorType::new(
            LabelType::Label("Loose".into()),
            PropertyType::Open({
                let mut m = BTreeMap::new();
                m.insert("name".to_string(), SimpleType::S);
                m
            }),
        );
        let s = format_node_descriptor(&d);
        assert_eq!(s, "(:Loose {name STRING, *})");
    }
}
