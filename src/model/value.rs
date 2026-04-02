use std::fmt;

/// A property value: int, string, or bool.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Str(String),
    Bool(bool),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Str(s) => write!(f, "\"{s}\""),
            Value::Bool(b) => write!(f, "{b}"),
        }
    }
}

/// A runtime value that can appear in a path or assignment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathValue {
    Node(String),
    EdgeDirectional(String),
    EdgeUndirectional(String),
    Nothing,
    List(Vec<PathValue>),
}

impl PathValue {
    /// Returns the id string for Node/Edge variants, None for Nothing/List.
    pub fn id(&self) -> Option<&str> {
        match self {
            PathValue::Node(id)
            | PathValue::EdgeDirectional(id)
            | PathValue::EdgeUndirectional(id) => Some(id),
            PathValue::Nothing | PathValue::List(_) => None,
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
            PathValue::Node(id) => write!(f, "{id}"),
            PathValue::EdgeDirectional(id) => write!(f, "{id}"),
            PathValue::EdgeUndirectional(id) => write!(f, "{id}"),
            PathValue::Nothing => write!(f, "Nothing"),
            PathValue::List(l) => {
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
    pub fn first_node_id(&self) -> Option<&str> {
        self.0.first().and_then(|pv| match pv {
            PathValue::Node(id) => Some(id.as_str()),
            _ => None,
        })
    }

    /// Get the ID of the last node in the path (paths always end with a node).
    pub fn last_node_id(&self) -> Option<&str> {
        self.0.last().and_then(|pv| match pv {
            PathValue::Node(id) => Some(id.as_str()),
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
            PathValue::Node("n1".into()),
            PathValue::EdgeDirectional("e1".into()),
            PathValue::Node("n2".into()),
        ]);
        let p2 = Path(vec![
            PathValue::Node("n2".into()),
            PathValue::EdgeDirectional("e2".into()),
            PathValue::Node("n3".into()),
        ]);
        let p3 = Path(vec![
            PathValue::Node("n3".into()),
            PathValue::EdgeDirectional("e3".into()),
            PathValue::Node("n4".into()),
        ]);

        assert!(p1.can_concat(&p2));
        assert!(!p1.can_concat(&p3));
    }

    #[test]
    fn test_path_concat() {
        let p1 = Path(vec![
            PathValue::Node("n1".into()),
            PathValue::EdgeDirectional("e1".into()),
            PathValue::Node("n2".into()),
        ]);
        let p2 = Path(vec![
            PathValue::Node("n2".into()),
            PathValue::EdgeDirectional("e2".into()),
            PathValue::Node("n3".into()),
        ]);
        let expected = Path(vec![
            PathValue::Node("n1".into()),
            PathValue::EdgeDirectional("e1".into()),
            PathValue::Node("n2".into()),
            PathValue::EdgeDirectional("e2".into()),
            PathValue::Node("n3".into()),
        ]);
        assert_eq!(p1.concat(&p2), expected);
    }

    #[test]
    fn test_pathvalue_display() {
        assert_eq!(PathValue::Node("n1".into()).to_string(), "n1");
        assert_eq!(PathValue::Nothing.to_string(), "Nothing");
        let list = PathValue::List(vec![
            PathValue::Node("n1".into()),
            PathValue::Node("n2".into()),
        ]);
        assert_eq!(list.to_string(), "[n1, n2]");
    }
}
