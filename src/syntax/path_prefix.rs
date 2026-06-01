use std::fmt;

/// ISO/IEC 39075:2024 §16.6 `<path mode>`. A path mode is *restrictive*:
/// it constrains which walks count as valid matches. `WALK` is the
/// default and imposes no restriction.
///
/// Feature flags (§16.6 Conformance Rules):
///   - `WALK`    → G010 "Explicit WALK keyword"
///   - `TRAIL`   → G011 "Advanced path modes: TRAIL"
///   - `SIMPLE`  → G012 "Advanced path modes: SIMPLE"
///   - `ACYCLIC` → G013 "Advanced path modes: ACYCLIC"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    /// Any walk; repeated nodes and edges allowed. Default.
    Walk,
    /// No repeated edges.
    Trail,
    /// No repeated nodes, except that the first and last node may coincide
    /// (a "simple cycle").
    Simple,
    /// No repeated nodes at all — the "no-cycles" mode.
    Acyclic,
}

/// ISO §16.6 `<path search prefix>`, normalized per Syntax Rule 2. A
/// search prefix is *selective*: it picks a subset of the matching paths
/// per `(left boundary node, right boundary node)` partition.
///
/// The standard's surface syntax is collapsed here into the normal form
/// from §16.6 SR 2c:
///   - `ALL`            → [`PathSearch::All`]
///   - `ANY [N]`        → `ANY N PATHS`         → [`PathSearch::Any`]
///   - `ANY SHORTEST`   → `SHORTEST 1 PATH`     → [`PathSearch::ShortestPaths`] `{ count: 1 }`
///   - `ALL SHORTEST`   → `SHORTEST 1 GROUP`    → [`PathSearch::ShortestGroups`] `{ count: 1 }`
///   - `SHORTEST N`     → `SHORTEST N PATHS`    → [`PathSearch::ShortestPaths`]
///   - `SHORTEST N GROUPS` → `SHORTEST N GROUPS`→ [`PathSearch::ShortestGroups`]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSearch {
    /// Every matching path. Default. (Feature G015 for the explicit `ALL`.)
    All,
    /// `ANY N` — `count` arbitrary paths per boundary partition. (G016.)
    Any { count: usize },
    /// `SHORTEST N [PATHS]` / `ANY SHORTEST` — the `count` shortest paths
    /// per partition, ranked by path length. (G018 any-shortest = `count 1`;
    /// G019 counted shortest path.)
    ShortestPaths { count: usize },
    /// `SHORTEST N GROUPS` / `ALL SHORTEST` — every path whose length is
    /// among the `count` shortest distinct lengths in its partition.
    /// (G017 all-shortest = `count 1` group; G020 counted shortest group.)
    ShortestGroups { count: usize },
}

/// ISO §16.6 `<path pattern prefix>` = `<path mode prefix>` (mode only) or
/// `<path search prefix>` (mode + selection). Carried on a MATCH clause and
/// applied by the runtime after the clause's paths are materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPrefix {
    pub mode: PathMode,
    pub search: PathSearch,
}

impl PathPrefix {
    /// `WALK ALL` — the implicit prefix every plain pattern carries. When a
    /// parsed prefix reduces to this, it imposes no restriction and the
    /// parser drops it (stores `None`) so the runtime can skip the
    /// materialize-and-filter pass entirely.
    pub fn is_trivial(&self) -> bool {
        self.mode == PathMode::Walk && self.search == PathSearch::All
    }
}

impl fmt::Display for PathMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathMode::Walk => "WALK",
            PathMode::Trail => "TRAIL",
            PathMode::Simple => "SIMPLE",
            PathMode::Acyclic => "ACYCLIC",
        })
    }
}

impl fmt::Display for PathPrefix {
    /// Prints the normalized form. It re-parses to a `PathPrefix` with the
    /// same semantics (e.g. `ALL SHORTEST` round-trips as `SHORTEST 1 GROUPS`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode_suffix = if self.mode == PathMode::Walk {
            String::new()
        } else {
            format!(" {}", self.mode)
        };
        match self.search {
            // A bare mode prefix (search defaults to ALL). `WALK ALL` is
            // trivial and never stored, so `mode` here is non-`Walk`.
            PathSearch::All => write!(f, "{}", self.mode),
            PathSearch::Any { count } => write!(f, "ANY {count}{mode_suffix}"),
            PathSearch::ShortestPaths { count } => {
                write!(f, "SHORTEST {count}{mode_suffix} PATHS")
            }
            PathSearch::ShortestGroups { count } => {
                write!(f, "SHORTEST {count}{mode_suffix} GROUPS")
            }
        }
    }
}
