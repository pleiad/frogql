//! Distance metrics for vector search.
//!
//! Every metric is a *distance*: smaller means nearer. Inner product is
//! therefore negated, so the "nearest" neighbour under `Ip` is the one
//! maximising the dot product. This invariant is what lets the top-k
//! sinks and the threshold cut in the in-LTJ strategy treat all three
//! uniformly.
//!
//! Cosine needs the L2 norm of both operands. Recomputing it per
//! comparison would dominate the inner loop, so `VectorSet` precomputes
//! one norm per stored row at load time (O(n·d) once) and the caller
//! precomputes the query norm once; both are passed in. Metrics that do
//! not need norms ignore the arguments.

/// Wire tag for the metric, persisted in the sidecar header.
pub const METRIC_L2SQ: u8 = 0;
pub const METRIC_COSINE: u8 = 1;
pub const METRIC_IP: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Squared euclidean distance. Monotone in the euclidean distance,
    /// so it induces the same neighbour order without the square root.
    L2Sq,
    /// `1 - cos(a, b)`, in `[0, 2]`. A zero-norm operand has no
    /// direction; we define its cosine distance as 1 (orthogonal).
    Cosine,
    /// Negated inner product. Not a metric in the mathematical sense
    /// (no triangle inequality, can be negative), but it is a valid
    /// ranking key, which is all the strategies need.
    Ip,
}

impl Metric {
    pub fn as_u8(self) -> u8 {
        match self {
            Metric::L2Sq => METRIC_L2SQ,
            Metric::Cosine => METRIC_COSINE,
            Metric::Ip => METRIC_IP,
        }
    }

    pub fn from_u8(tag: u8) -> Option<Metric> {
        match tag {
            METRIC_L2SQ => Some(Metric::L2Sq),
            METRIC_COSINE => Some(Metric::Cosine),
            METRIC_IP => Some(Metric::Ip),
            _ => None,
        }
    }

    /// Parse the CLI / env spelling.
    pub fn parse(s: &str) -> Option<Metric> {
        match s.to_ascii_lowercase().as_str() {
            "l2" | "l2sq" | "euclidean" => Some(Metric::L2Sq),
            "cosine" | "cos" => Some(Metric::Cosine),
            "ip" | "dot" | "inner" => Some(Metric::Ip),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Metric::L2Sq => "l2sq",
            Metric::Cosine => "cosine",
            Metric::Ip => "ip",
        }
    }

    /// Does this metric read the precomputed norms?
    pub fn needs_norms(self) -> bool {
        matches!(self, Metric::Cosine)
    }

    /// Distance between `a` and `b`. `a_norm` / `b_norm` are the L2
    /// norms of the operands, consulted only by `Cosine`.
    ///
    /// Panics in debug builds if the dimensions disagree; callers are
    /// expected to have validated the query length against
    /// `VectorSet::dim` once, not once per comparison.
    pub fn dist(self, a: &[f32], a_norm: f32, b: &[f32], b_norm: f32) -> f32 {
        debug_assert_eq!(a.len(), b.len(), "dimension mismatch");
        match self {
            Metric::L2Sq => {
                let mut acc = 0.0f32;
                for (x, y) in a.iter().zip(b.iter()) {
                    let d = x - y;
                    acc += d * d;
                }
                acc
            }
            Metric::Cosine => {
                if a_norm == 0.0 || b_norm == 0.0 {
                    return 1.0;
                }
                1.0 - dot(a, b) / (a_norm * b_norm)
            }
            Metric::Ip => -dot(a, b),
        }
    }
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        acc += x * y;
    }
    acc
}

/// L2 norm of `v`.
pub fn norm(v: &[f32]) -> f32 {
    dot(v, v).sqrt()
}

/// Total order over `f32` distances. `f32` is not `Ord`, and the crate
/// deliberately ships no `ordered-float` dependency, so every heap and
/// sort over distances routes through here. `total_cmp` is stable since
/// 1.62, below the 1.71 MSRV.
pub fn cmp_dist(a: f32, b: f32) -> std::cmp::Ordering {
    a.total_cmp(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2sq_is_zero_on_identical() {
        let v = [1.0, 2.0, 3.0];
        assert_eq!(Metric::L2Sq.dist(&v, 0.0, &v, 0.0), 0.0);
    }

    #[test]
    fn l2sq_matches_hand_computation() {
        let a = [0.0, 0.0];
        let b = [3.0, 4.0];
        assert_eq!(Metric::L2Sq.dist(&a, 0.0, &b, 0.0), 25.0);
    }

    #[test]
    fn cosine_is_zero_on_parallel_vectors() {
        let a = [1.0, 1.0];
        let b = [2.0, 2.0];
        let d = Metric::Cosine.dist(&a, norm(&a), &b, norm(&b));
        assert!(d.abs() < 1e-6, "expected ~0, got {d}");
    }

    #[test]
    fn cosine_is_one_on_orthogonal_vectors() {
        let a = [1.0, 0.0];
        let b = [0.0, 1.0];
        let d = Metric::Cosine.dist(&a, norm(&a), &b, norm(&b));
        assert!((d - 1.0).abs() < 1e-6, "expected ~1, got {d}");
    }

    #[test]
    fn cosine_is_two_on_opposite_vectors() {
        let a = [1.0, 0.0];
        let b = [-1.0, 0.0];
        let d = Metric::Cosine.dist(&a, norm(&a), &b, norm(&b));
        assert!((d - 2.0).abs() < 1e-6, "expected ~2, got {d}");
    }

    #[test]
    fn cosine_treats_zero_vector_as_orthogonal() {
        let a = [0.0, 0.0];
        let b = [1.0, 0.0];
        assert_eq!(Metric::Cosine.dist(&a, 0.0, &b, norm(&b)), 1.0);
    }

    #[test]
    fn ip_is_negated_so_larger_dot_is_nearer() {
        let q = [1.0, 0.0];
        let near = [5.0, 0.0];
        let far = [1.0, 0.0];
        assert!(Metric::Ip.dist(&q, 0.0, &near, 0.0) < Metric::Ip.dist(&q, 0.0, &far, 0.0));
    }

    #[test]
    fn tags_round_trip() {
        for m in [Metric::L2Sq, Metric::Cosine, Metric::Ip] {
            assert_eq!(Metric::from_u8(m.as_u8()), Some(m));
            assert_eq!(Metric::parse(m.name()), Some(m));
        }
        assert_eq!(Metric::from_u8(200), None);
        assert_eq!(Metric::parse("manhattan"), None);
    }

    #[test]
    fn cmp_dist_orders_nan_last() {
        let mut v = [1.0f32, f32::NAN, 0.5];
        v.sort_by(|a, b| cmp_dist(*a, *b));
        assert_eq!(v[0], 0.5);
        assert_eq!(v[1], 1.0);
        assert!(v[2].is_nan());
    }
}
