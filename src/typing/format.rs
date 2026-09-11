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

/// Short names for a schema's node types, so an edge line can point at
/// one instead of repeating its whole record.
///
/// A node type with a dozen properties is printed once per edge that
/// touches it, and a schema with seven edges off one hub prints that hub
/// seven times. The information is all there and none of it is legible:
/// the part that differs between two lines — the edge label — is the
/// short part, buried between two long ones.
///
/// The name comes from the labels (`Copiloto&Persona` → `copiloto_persona`),
/// so it is stable across runs and says what it names. A type whose label
/// is not a plain conjunction — a union, or the wildcard — has no such
/// name and takes a positional one.
///
/// Keyed on the *rendering* rather than on the descriptor: an edge
/// endpoint is a separate value from the node type it matches, and what
/// makes them the same type for a reader is that they print the same.
pub struct NodeTypeNames {
    by_rendering: BTreeMap<String, String>,
}

impl NodeTypeNames {
    /// Names for every node type in `schema`.
    pub fn of(schema: &Schema) -> Self {
        let mut by_rendering: BTreeMap<String, String> = BTreeMap::new();
        let mut taken: BTreeMap<String, usize> = BTreeMap::new();
        for (i, vt) in schema.nodes.iter().enumerate() {
            let VariableType::Node(d) = vt else { continue };
            let rendering = format_node_descriptor(d);
            if by_rendering.contains_key(&rendering) {
                continue;
            }
            let base = name_from_labels(&d.label).unwrap_or_else(|| format!("t{}", i + 1));
            // Two distinct types can still want the same name — a
            // user-written schema may declare one label twice with
            // different records. Numbering keeps the reference unambiguous.
            let n = taken.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { base } else { format!("{base}_{n}") };
            by_rendering.insert(rendering, name);
        }
        NodeTypeNames { by_rendering }
    }

    /// The name for an endpoint, or `None` when it is not one of the
    /// schema's declared node types — an endpoint can carry a label
    /// combination that never appears standalone, and inventing a name
    /// for something the reader cannot look up would be worse than
    /// printing it in full.
    pub fn get(&self, d: &DescriptorType) -> Option<&str> {
        self.by_rendering
            .get(&format_node_descriptor(d))
            .map(|s| s.as_str())
    }

    fn is_empty(&self) -> bool {
        self.by_rendering.is_empty()
    }
}

/// `Copiloto&Persona` → `copiloto_persona`. `None` for a label that is
/// not a plain conjunction of names.
pub fn name_from_labels(label: &LabelType) -> Option<String> {
    let parts = label.required_labels();
    if parts.is_empty() {
        return None;
    }
    let mut name = String::new();
    for p in parts {
        if !name.is_empty() {
            name.push('_');
        }
        for c in p.chars() {
            if c.is_alphanumeric() {
                name.extend(c.to_lowercase());
            } else {
                name.push('_');
            }
        }
    }
    Some(name)
}

/// Render `schema` as a CREATE GRAPH TYPE body. Returns a multi-line
/// string with one element per line, indented for readability.
///
/// Node types are given names once the schema has edges to reference
/// them from; with no edges a name has no reader and is only noise. The
/// named form does **not** re-parse as a CREATE body — `(fpl)` is a name
/// where the grammar wants a label — which is the price of the edge list
/// being readable at all. An unnamed schema (no edges, or endpoints that
/// are not declared node types) still round-trips.
pub fn format_schema(schema: &Schema) -> String {
    let mut out = String::new();
    let names = if schema.edges.is_empty() {
        None
    } else {
        let n = NodeTypeNames::of(schema);
        (!n.is_empty()).then_some(n)
    };
    if !schema.nodes.is_empty() {
        out.push_str("Node types:\n");
        for vt in schema.nodes.iter() {
            out.push_str("    ");
            if let (Some(names), VariableType::Node(d)) = (&names, vt) {
                if let Some(name) = names.get(d) {
                    out.push_str(name);
                    out.push_str(" = ");
                }
            }
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
            out.push_str(&format_variable_with(vt, names.as_ref()));
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
    format_variable_with(vt, None)
}

/// As `format_variable`, but abbreviating any endpoint `names` has a
/// name for.
pub fn format_variable_with(vt: &VariableType, names: Option<&NodeTypeNames>) -> String {
    match vt {
        VariableType::Node(d) => format_node_descriptor(d),
        VariableType::EdgeDirectional { desc, left, right } => format!(
            "{}-[{}]->{}",
            format_endpoint_with(left, names),
            format_edge_descriptor(desc),
            format_endpoint_with(right, names),
        ),
        VariableType::EdgeNonDirectional { desc, left, right } => format!(
            "{}~[{}]~{}",
            format_endpoint_with(left, names),
            format_edge_descriptor(desc),
            format_endpoint_with(right, names),
        ),
        // Unions / Group / Zero appear in inference output too, even
        // though CREATE syntax doesn't accept them at the top level.
        // Render best-effort so SHOW never blanks out.
        VariableType::Union(a, b) => format!("({}) | ({})", format_variable(a), format_variable(b)),
        VariableType::Group(t, _) => format!("group<{}>", format_variable(t)),
        VariableType::Null => "Null".to_string(),
        VariableType::Path => "PATH".to_string(),
        VariableType::Scalar(t) => format_simple_type(t),
        VariableType::Zero => "⊥".to_string(),
    }
}

fn format_endpoint_with(vt: &VariableType, names: Option<&NodeTypeNames>) -> String {
    match vt {
        VariableType::Node(d) => match names.and_then(|n| n.get(d)) {
            Some(name) => format!("({name})"),
            None => format_node_descriptor(d),
        },
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
        SimpleType::Null => "NULL".to_string(),
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
        SimpleType::Date => "DATE".to_string(),
        SimpleType::LocalDatetime => "LOCAL DATETIME".to_string(),
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
