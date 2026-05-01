use std::fmt;

/// A property value. Scalars (Int/Float/Str/Bool/Null) plus the GQL
/// constructed types List (ordered collection) and Record (named fields, can
/// nest). `Null` is the SQL-style absent value; in general `WHERE` expressions,
/// comparisons involving null follow SQL three-valued logic. Aggregators skip null,
/// and it is distinct from any string, including `"NULL"`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
    Record(std::collections::BTreeMap<String, Value>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => {
                if x.is_finite() && x.fract() == 0.0 {
                    write!(f, "{x:.1}")
                } else {
                    write!(f, "{x}")
                }
            }
            Value::Str(s) => write!(f, "\"{s}\""),
            Value::Bool(b) => write!(f, "{b}"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| format!("{v}")).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Record(fields) => {
                let parts: Vec<String> = fields.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
        }
    }
}

/// Internal ID type for nodes and edges.
pub type Id = u32;

/// A runtime value that can appear in a path or assignment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathValue {
    Node(Id),
    EdgeDirectional(Id),
    EdgeUndirectional(Id),
    Nothing,
    /// Repetition grouping produced by `{n,m}` quantifiers. NOT a user-facing list;
    /// user lists live in `Value::List` once phase-1 list-values land.
    Group(Vec<PathValue>),
}

impl PathValue {
    /// Returns the internal id for Node/Edge variants, None for Nothing.
    pub fn id(&self) -> Option<Id> {
        match self {
            PathValue::Node(id)
            | PathValue::EdgeDirectional(id)
            | PathValue::EdgeUndirectional(id) => Some(*id),
            PathValue::Nothing | PathValue::Group(_) => None,
        }
    }

    pub fn is_node(&self) -> bool {
        matches!(self, PathValue::Node(_))
    }

    pub fn is_edge(&self) -> bool {
        matches!(
            self,
            PathValue::EdgeDirectional(_) | PathValue::EdgeUndirectional(_)
        )
    }
}

impl fmt::Display for PathValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathValue::Node(id) => write!(f, "n{id}"),
            PathValue::EdgeDirectional(id) => write!(f, "e{id}"),
            PathValue::EdgeUndirectional(id) => write!(f, "u{id}"),
            PathValue::Nothing => write!(f, "Nothing"),
            PathValue::Group(l) => {
                let items: Vec<String> = l.iter().map(|x| x.to_string()).collect();
                write!(f, "[{}]", items.join(", "))
            }
        }
    }
}

/// A path is a sequence of alternating nodes and edges: [n1, e1, n2, e2, n3, ...].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path(pub Vec<PathValue>);

impl Path {
    /// Get the ID of the first node in the path (paths always start with a node).
    pub fn first_node_id(&self) -> Option<Id> {
        self.0.first().and_then(|pv| match pv {
            PathValue::Node(id) => Some(*id),
            _ => None,
        })
    }

    /// Get the ID of the last node in the path (paths always end with a node).
    pub fn last_node_id(&self) -> Option<Id> {
        self.0.last().and_then(|pv| match pv {
            PathValue::Node(id) => Some(*id),
            _ => None,
        })
    }

    /// Can this path be concatenated with `other`?
    /// The last element of self must equal the first element of other.
    pub fn can_concat(&self, other: &Path) -> bool {
        match (self.0.last(), other.0.first()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Concatenate two paths, skipping the duplicate middle node.
    pub fn concat(&self, other: &Path) -> Path {
        debug_assert!(self.can_concat(other));
        let mut result = self.0.clone();
        result.extend_from_slice(&other.0[1..]);
        Path(result)
    }

    /// Cross-product of two paths: concatenation of both path elements.
    /// Used for Join (Q1, Q2) semantics where paths are independent.
    pub fn cross(&self, other: &Path) -> Path {
        let mut result = self.0.clone();
        result.extend_from_slice(&other.0);
        Path(result)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let items: Vec<String> = self.0.iter().map(|x| x.to_string()).collect();
        write!(f, "{}", items.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_can_concat() {
        let p1 = Path(vec![
            PathValue::Node(1),
            PathValue::EdgeDirectional(10),
            PathValue::Node(2),
        ]);
        let p2 = Path(vec![
            PathValue::Node(2),
            PathValue::EdgeDirectional(20),
            PathValue::Node(3),
        ]);
        let p3 = Path(vec![
            PathValue::Node(3),
            PathValue::EdgeDirectional(30),
            PathValue::Node(4),
        ]);

        assert!(p1.can_concat(&p2));
        assert!(!p1.can_concat(&p3));
    }

    #[test]
    fn test_path_concat() {
        let p1 = Path(vec![
            PathValue::Node(1),
            PathValue::EdgeDirectional(10),
            PathValue::Node(2),
        ]);
        let p2 = Path(vec![
            PathValue::Node(2),
            PathValue::EdgeDirectional(20),
            PathValue::Node(3),
        ]);
        let expected = Path(vec![
            PathValue::Node(1),
            PathValue::EdgeDirectional(10),
            PathValue::Node(2),
            PathValue::EdgeDirectional(20),
            PathValue::Node(3),
        ]);
        assert_eq!(p1.concat(&p2), expected);
    }

    #[test]
    fn test_pathvalue_display() {
        assert_eq!(PathValue::Node(1).to_string(), "n1");
        assert_eq!(PathValue::Nothing.to_string(), "Nothing");
    }
}
