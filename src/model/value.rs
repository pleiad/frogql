use std::fmt;

/// A property or projected value. ISO §4.4 distinguishes scalar values
/// (`Int`/`Float`/`Str`/`Bool`), structured values (`List`, `Record`),
/// the null value (`Null`), and reference values (`Node`, `Edge`).
///
/// Per §4.4.4 reference values encapsulate the global object identifier
/// — `Node(id)` and `Edge(id)` are opaque identity values. Equality is
/// defined as "iff they refer to the same referent" (same id). They are
/// not orderable without Feature GA04. Properties are read from the
/// graph by id at attribute-lookup time, never carried with the value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
    Record(std::collections::BTreeMap<String, Value>),
    Node(Id),
    Edge(Id),
    /// ISO §4.4 PATH value — a materialized path bound to a path
    /// variable (`MATCH p = (a)-[:k]->(b)`). The sequence alternates
    /// node and edge reference values in match order
    /// (`[Node, Edge, Node, ...]`); the path functions of §20.16
    /// (`ELEMENTS`/`PATH_LENGTH`/`CARDINALITY`) read it. Never a
    /// property value: a path cannot be stored on a node or edge.
    Path(Vec<Value>),
    /// ISO §4.16.6 DATE value, constructed by the §20.27 `DATE(...)`
    /// function. Days since 1970-01-01, proleptic Gregorian. Query-time
    /// value only in this phase: like `Path`, it is never a property
    /// value (temporal property storage is future work).
    Date(i32),
    /// ISO §4.16.6 LOCAL DATETIME value, constructed by the §20.27
    /// `LOCAL_DATETIME(...)` function. Milliseconds since
    /// 1970-01-01T00:00:00, no timezone (the ZONED flavors are future
    /// work). Query-time value only, like `Date`.
    LocalDatetime(i64),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian civil date.
/// Howard Hinnant's `days_from_civil` algorithm.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = ((m + 9) % 12) as u64; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + (d as u64 - 1); // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe as i64 - 719468
}

/// Inverse of `days_from_civil`: `(year, month, day)` for days since epoch.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Whether `(y, m, d)` is a real proleptic-Gregorian calendar date.
pub fn valid_civil(y: i64, m: u32, d: u32) -> bool {
    if !(1..=12).contains(&m) || d == 0 {
        return false;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => unreachable!(),
    };
    d <= dim
}

/// Parse an ISO-8601 `<date string>` (`YYYY-MM-DD`) to days since epoch.
pub fn parse_date_str(s: &str) -> Option<i32> {
    let mut it = s.split('-');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d) = (
        y.parse::<i64>().ok()?,
        m.parse::<u32>().ok()?,
        d.parse::<u32>().ok()?,
    );
    if !valid_civil(y, m, d) {
        return None;
    }
    i32::try_from(days_from_civil(y, m, d)).ok()
}

/// Parse an ISO-8601 `<datetime string>` (`YYYY-MM-DDTHH:MM[:SS[.mmm]]`)
/// to milliseconds since epoch, no timezone.
pub fn parse_datetime_str(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let days = parse_date_str(date)? as i64;
    let mut parts = time.split(':');
    let (h, mi) = (
        parts.next()?.parse::<u32>().ok()?,
        parts.next()?.parse::<u32>().ok()?,
    );
    let (sec, ms) = match parts.next() {
        None => (0u32, 0u32),
        Some(sec_part) => {
            if parts.next().is_some() {
                return None;
            }
            match sec_part.split_once('.') {
                None => (sec_part.parse::<u32>().ok()?, 0),
                Some((s2, frac)) => {
                    if frac.is_empty()
                        || frac.len() > 3
                        || !frac.bytes().all(|b| b.is_ascii_digit())
                    {
                        return None;
                    }
                    let scale = 10u32.pow(3 - frac.len() as u32);
                    (s2.parse::<u32>().ok()?, frac.parse::<u32>().ok()? * scale)
                }
            }
        }
    };
    if h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    Some(
        days * 86_400_000
            + (h as i64) * 3_600_000
            + (mi as i64) * 60_000
            + (sec as i64) * 1000
            + ms as i64,
    )
}

/// Format days-since-epoch as `YYYY-MM-DD`.
pub fn format_date(days: i32) -> String {
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Format millis-since-epoch as `YYYY-MM-DDTHH:MM:SS[.mmm]`.
pub fn format_datetime(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let rem = millis.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let (h, mi, sec, ms) = (
        rem / 3_600_000,
        rem % 3_600_000 / 60_000,
        rem % 60_000 / 1000,
        rem % 1000,
    );
    if ms == 0 {
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{sec:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{sec:02}.{ms:03}")
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
            Value::Node(id) => write!(f, "n{id}"),
            Value::Edge(id) => write!(f, "e{id}"),
            Value::Path(items) => {
                let parts: Vec<String> = items.iter().map(|v| format!("{v}")).collect();
                write!(f, "<{}>", parts.join(", "))
            }
            Value::Date(days) => write!(f, "{}", format_date(*days)),
            Value::LocalDatetime(ms) => write!(f, "{}", format_datetime(*ms)),
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
    /// A materialized named path (`MATCH p = ...`) bound in the
    /// assignment. The sequence alternates node and edge values in match
    /// order, exactly as it appears in the row's `Path`. Distinct from
    /// `Group`: a path projects to `Value::Path`, a group to `Value::List`.
    Path(Vec<PathValue>),
}

impl PathValue {
    /// Returns the internal id for Node/Edge variants, None for Nothing.
    pub fn id(&self) -> Option<Id> {
        match self {
            PathValue::Node(id)
            | PathValue::EdgeDirectional(id)
            | PathValue::EdgeUndirectional(id) => Some(*id),
            PathValue::Nothing | PathValue::Group(_) | PathValue::Path(_) => None,
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
            PathValue::Path(l) => {
                let items: Vec<String> = l.iter().map(|x| x.to_string()).collect();
                write!(f, "<{}>", items.join(", "))
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
