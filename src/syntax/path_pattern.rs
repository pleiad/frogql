use std::collections::HashSet;
use std::fmt;

use super::descriptor::Descriptor;
use super::expr::Expr;
use super::path_prefix::PathPrefix;

/// Path patterns — the core of GQL queries.
#[derive(Debug, Clone, PartialEq)]
pub enum PathPattern {
    Node(Option<Descriptor>),
    EdgeRight(Option<Descriptor>),
    EdgeLeft(Option<Descriptor>),
    EdgeUndirected(Option<Descriptor>),
    EdgeAnyDirection(Option<Descriptor>),
    Concat(Box<PathPattern>, Box<PathPattern>),
    Union(Box<PathPattern>, Box<PathPattern>),
    Filter(Box<PathPattern>, Expr),
    Repeat {
        pattern: Box<PathPattern>,
        lb: usize,
        ub: Option<usize>,
    },
    Questioned(Box<PathPattern>),
    /// Join of two queries: Q1, Q2
    /// Semantics: cross-product of paths, keeping only rows where assignments unify.
    /// Result paths are paired (p1 × p2), assignment is mu1 ∪ mu2.
    Join(Box<PathPattern>, Box<PathPattern>),
    /// ISO §16.6 prefix scoped to one `<path pattern>` operand.
    Selected {
        prefix: PathPrefix,
        pattern: Box<PathPattern>,
    },
    /// ISO `<path variable declaration>` — `p = <path pattern>`. Binds the
    /// whole operand's matched path to the variable `var`, materialized as
    /// a `Value::Path` for the §20.16 path functions. Wraps one comma
    /// operand, outside any `Selected` prefix
    /// (`Named { var, Selected { prefix, pattern } }`). Transparent to the
    /// pattern's own variable types; it only adds the path binding.
    Named {
        var: String,
        pattern: Box<PathPattern>,
    },
}

impl PathPattern {
    /// Collect all free variable names in this pattern.
    pub fn freevars(&self) -> HashSet<String> {
        match self {
            PathPattern::Node(Some(d)) => d.var.iter().cloned().collect(),
            PathPattern::Node(None) => HashSet::new(),
            PathPattern::EdgeRight(d)
            | PathPattern::EdgeLeft(d)
            | PathPattern::EdgeUndirected(d)
            | PathPattern::EdgeAnyDirection(d) => {
                d.as_ref().and_then(|d| d.var.clone()).into_iter().collect()
            }
            PathPattern::Concat(p1, p2)
            | PathPattern::Union(p1, p2)
            | PathPattern::Join(p1, p2) => {
                let mut s = p1.freevars();
                s.extend(p2.freevars());
                s
            }
            PathPattern::Filter(p, _) => p.freevars(),
            PathPattern::Repeat { pattern, .. } => pattern.freevars(),
            PathPattern::Questioned(p) => p.freevars(),
            PathPattern::Selected { pattern, .. } => pattern.freevars(),
            // The path variable is not a node/edge pattern variable: it
            // never unifies across a join. Expose only the inner element
            // variables here; the path binding lives in the environment
            // (typecheck) and the assignment (runtime).
            PathPattern::Named { pattern, .. } => pattern.freevars(),
        }
    }

    /// Whether this pattern contains a prefixed operand.
    pub fn has_selected(&self) -> bool {
        match self {
            PathPattern::Selected { .. } => true,
            PathPattern::Node(_)
            | PathPattern::EdgeRight(_)
            | PathPattern::EdgeLeft(_)
            | PathPattern::EdgeUndirected(_)
            | PathPattern::EdgeAnyDirection(_) => false,
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                a.has_selected() || b.has_selected()
            }
            PathPattern::Filter(p, _)
            | PathPattern::Questioned(p)
            | PathPattern::Repeat { pattern: p, .. }
            | PathPattern::Named { pattern: p, .. } => p.has_selected(),
        }
    }

    /// Whether this pattern contains an undirected edge (`~[...]~`).
    ///
    /// Pinning both endpoints of an undirected edge in the LTJ decomposition
    /// does not constrain the match to that specific pair, so the
    /// per-outer-row pinned probes (correlated EXISTS / VALUE subquery) must
    /// not fire on a body that contains one — they fall back to the global
    /// materialisation instead.
    pub fn has_undirected_edge(&self) -> bool {
        match self {
            PathPattern::EdgeUndirected(_) => true,
            PathPattern::Node(_)
            | PathPattern::EdgeRight(_)
            | PathPattern::EdgeLeft(_)
            | PathPattern::EdgeAnyDirection(_) => false,
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                a.has_undirected_edge() || b.has_undirected_edge()
            }
            PathPattern::Filter(p, _)
            | PathPattern::Questioned(p)
            | PathPattern::Repeat { pattern: p, .. }
            | PathPattern::Selected { pattern: p, .. }
            | PathPattern::Named { pattern: p, .. } => p.has_undirected_edge(),
        }
    }

    /// Get the descriptor ref (for node/edge patterns).
    pub fn descriptor(&self) -> Option<&Descriptor> {
        match self {
            PathPattern::Node(d) => d.as_ref(),
            PathPattern::EdgeRight(d)
            | PathPattern::EdgeLeft(d)
            | PathPattern::EdgeUndirected(d)
            | PathPattern::EdgeAnyDirection(d) => d.as_ref(),
            _ => None,
        }
    }

    /// Collect the lower bounds of every *unbounded* repetition (`ub ==
    /// None`, i.e. `*`, `+`, `{n,}`) anywhere in this pattern. An empty
    /// result means the pattern is fully bounded. The typechecker uses
    /// this to gate unbounded repetition behind a SHORTEST-family search
    /// prefix or a restrictive path mode.
    pub fn unbounded_repeat_lbs(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect_unbounded_lbs(&mut out);
        out
    }

    fn collect_unbounded_lbs(&self, out: &mut Vec<usize>) {
        match self {
            PathPattern::Node(_)
            | PathPattern::EdgeRight(_)
            | PathPattern::EdgeLeft(_)
            | PathPattern::EdgeUndirected(_)
            | PathPattern::EdgeAnyDirection(_) => {}
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                a.collect_unbounded_lbs(out);
                b.collect_unbounded_lbs(out);
            }
            PathPattern::Filter(p, _) | PathPattern::Questioned(p) => p.collect_unbounded_lbs(out),
            PathPattern::Selected { pattern, .. } | PathPattern::Named { pattern, .. } => {
                pattern.collect_unbounded_lbs(out)
            }
            PathPattern::Repeat { pattern, lb, ub } => {
                if ub.is_none() {
                    out.push(*lb);
                }
                pattern.collect_unbounded_lbs(out);
            }
        }
    }
}

impl fmt::Display for PathPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathPattern::Node(Some(d)) => write!(f, "({d})"),
            PathPattern::Node(None) => write!(f, "()"),
            PathPattern::EdgeRight(Some(d)) => write!(f, "-[{d}]->"),
            PathPattern::EdgeRight(None) => write!(f, "-->"),
            PathPattern::EdgeLeft(Some(d)) => write!(f, "<-[{d}]-"),
            PathPattern::EdgeLeft(None) => write!(f, "<--"),
            PathPattern::EdgeUndirected(Some(d)) => write!(f, "~[{d}]~"),
            PathPattern::EdgeUndirected(None) => write!(f, "~~"),
            PathPattern::EdgeAnyDirection(Some(d)) => write!(f, "-[{d}]-"),
            PathPattern::EdgeAnyDirection(None) => write!(f, "--"),
            PathPattern::Concat(p1, p2) => write!(f, "{p1} {p2}"),
            PathPattern::Union(p1, p2) => write!(f, "({p1} | {p2})"),
            PathPattern::Filter(p, e) => write!(f, "{p} WHERE {e}"),
            PathPattern::Repeat {
                pattern,
                lb,
                ub: Some(ub),
            } => {
                write!(f, "({pattern}){{{lb},{ub}}}")
            }
            PathPattern::Repeat {
                pattern,
                lb,
                ub: None,
            } => {
                write!(f, "({pattern}){{{lb},}}")
            }
            PathPattern::Questioned(p) => write!(f, "({p})?"),
            PathPattern::Join(p1, p2) => write!(f, "{p1}, {p2}"),
            PathPattern::Selected { prefix, pattern } => write!(f, "{prefix} {pattern}"),
            PathPattern::Named { var, pattern } => write!(f, "{var} = {pattern}"),
        }
    }
}
