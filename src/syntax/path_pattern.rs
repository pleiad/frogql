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
    /// ISO §16.6 `<path pattern>` = `<path pattern prefix> <path pattern
    /// expression>`. A path-mode/path-search prefix scoped to exactly one
    /// `<path pattern>` (one operand of a `<path pattern list>`), per the
    /// standard's structure. The prefix selects/filters the paths matched
    /// by `pattern`, which — being a *selective* or *restrictive*
    /// `<path pattern>` — is evaluated in isolation (§16.6 NOTE 233):
    /// the runtime materializes `pattern`'s paths, then applies the mode
    /// filter and the search selection.
    Selected {
        prefix: PathPrefix,
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
        }
    }

    /// True if this pattern contains any `Selected` node (a `<path pattern
    /// prefix>`-decorated operand). Used to gate the collapsed/LTJ fast
    /// path and the OPTIONAL bind-pushdown: a selective/restrictive pattern
    /// must be evaluated in isolation, so callers keep it intact.
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
            | PathPattern::Repeat { pattern: p, .. } => p.has_selected(),
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
    /// this to gate unbounded repetition behind a single-SHORTEST search
    /// prefix (the only form the BFS runtime can evaluate in finite time
    /// on cyclic graphs) and to reject `{n,}` with `n >= 2`.
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
            PathPattern::Selected { pattern, .. } => pattern.collect_unbounded_lbs(out),
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
        }
    }
}
