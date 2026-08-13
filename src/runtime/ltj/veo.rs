/// Variable Elimination Order — determines the order in which variables are bound.
pub trait Veo {
    /// Get the next variable to bind at level `j`.
    fn var_at(&self, j: usize) -> u8;
    /// Total number of variables.
    fn size(&self) -> usize;
}

/// Simple VEO: fixed order determined at construction time. Sort key:
/// non-lonely variables first (they drive the join), then variables ordered
/// by estimated cardinality so a heavily filtered candidate binds before an
/// unfiltered one within each group.
///
/// Lonely-last is preserved as the primary key because, in the absence of
/// secondary indexes on property values, a strong filter on a lonely variable
/// (e.g. `p.id = K`) still requires a full enumeration of the variable's
/// position before the filter can reject — the eq does not become a true
/// point lookup. Letting it elevate above a non-lonely connector trades a
/// cheap structural intersection for a per-row scan.
pub struct VeoSimple {
    order: Vec<u8>,
}

impl VeoSimple {
    /// Build a VEO. `var_info[i]` is `(var_id, weight, is_lonely)`. Sort:
    /// non-lonely first, then ascending weight as a tiebreaker.
    pub fn new(mut var_info: Vec<(u8, usize, bool)>) -> Self {
        var_info.sort_by(|a, b| {
            a.2.cmp(&b.2) // lonely last
                .then(a.1.cmp(&b.1)) // ascending weight within each group
        });
        VeoSimple {
            order: var_info.into_iter().map(|(var_id, _, _)| var_id).collect(),
        }
    }
}

impl Veo for VeoSimple {
    fn var_at(&self, j: usize) -> u8 {
        self.order[j]
    }

    fn size(&self) -> usize {
        self.order.len()
    }
}

/// A base order with one variable moved to a chosen level.
///
/// The in-LTJ vector-search strategy needs to control where the search
/// variable binds: at level 0 the neighbour stream drives the whole
/// join, and deeper down the candidate set at each visit is already
/// narrowed by everything above it. Which of those wins is the question
/// the benchmark exists to answer, so the position has to be a knob.
///
/// This deliberately overrides the lonely-last rule documented above.
/// Correctness is unaffected — leapfrog is order-agnostic — but the
/// "level" axis of the benchmark is partly measuring how much that
/// heuristic was worth.
pub struct VeoOverride {
    order: Vec<u8>,
}

impl VeoOverride {
    /// Move `var` to `level`, keeping everything else in relative order.
    /// `None` when `var` is not in the base order, which happens when it
    /// was folded to a constant by the secondary index and so has no
    /// level to occupy.
    pub fn pin_at(base: &dyn Veo, var: u8, level: usize) -> Option<VeoOverride> {
        let mut order: Vec<u8> = (0..base.size()).map(|j| base.var_at(j)).collect();
        let cur = order.iter().position(|&v| v == var)?;
        let v = order.remove(cur);
        order.insert(level.min(order.len()), v);
        Some(VeoOverride { order })
    }

    /// Where `var` actually landed. The requested level is clamped, so
    /// callers must read the real position back rather than assume it.
    pub fn level_of(&self, var: u8) -> Option<usize> {
        self.order.iter().position(|&v| v == var)
    }

    /// The deepest sensible level: just before the first lonely variable.
    /// Past that point the search variable would bind after variables
    /// that only a full enumeration can produce, so the neighbour stream
    /// would no longer be narrowing anything.
    pub fn max_level(var_info: &[(u8, usize, bool)]) -> usize {
        var_info.iter().filter(|(_, _, lonely)| !*lonely).count()
    }
}

impl Veo for VeoOverride {
    fn var_at(&self, j: usize) -> u8 {
        self.order[j]
    }

    fn size(&self) -> usize {
        self.order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order_of(v: &dyn Veo) -> Vec<u8> {
        (0..v.size()).map(|j| v.var_at(j)).collect()
    }

    #[test]
    fn simple_puts_lonely_variables_last() {
        // (var, weight, lonely)
        let v = VeoSimple::new(vec![(0, 100, true), (1, 50, false), (2, 10, false)]);
        assert_eq!(order_of(&v), vec![2, 1, 0]);
    }

    #[test]
    fn override_moves_a_variable_to_the_front() {
        let base = VeoSimple::new(vec![(0, 10, false), (1, 20, false), (2, 30, false)]);
        let o = VeoOverride::pin_at(&base, 2, 0).expect("var 2 is in the order");
        assert_eq!(order_of(&o), vec![2, 0, 1]);
        assert_eq!(o.level_of(2), Some(0));
    }

    #[test]
    fn override_preserves_the_relative_order_of_the_rest() {
        let base = VeoSimple::new(vec![(0, 10, false), (1, 20, false), (2, 30, false)]);
        let o = VeoOverride::pin_at(&base, 0, 2).expect("present");
        assert_eq!(order_of(&o), vec![1, 2, 0]);
    }

    #[test]
    fn override_clamps_a_level_past_the_end() {
        let base = VeoSimple::new(vec![(0, 10, false), (1, 20, false)]);
        let o = VeoOverride::pin_at(&base, 0, 99).expect("present");
        assert_eq!(order_of(&o), vec![1, 0]);
        assert_eq!(o.level_of(0), Some(1), "read the real position back");
    }

    #[test]
    fn override_is_none_for_a_variable_that_was_folded_away() {
        let base = VeoSimple::new(vec![(0, 10, false)]);
        assert!(VeoOverride::pin_at(&base, 7, 0).is_none());
    }

    #[test]
    fn max_level_stops_before_the_first_lonely_variable() {
        let info = vec![(0, 1, false), (1, 1, false), (2, 1, true)];
        assert_eq!(VeoOverride::max_level(&info), 2);
        assert_eq!(VeoOverride::max_level(&[(0, 1, true)]), 0);
    }

    #[test]
    fn override_keeps_the_size_so_filter_placement_still_lines_up() {
        let base = VeoSimple::new(vec![(0, 10, false), (1, 20, false), (2, 30, true)]);
        let o = VeoOverride::pin_at(&base, 2, 0).expect("present");
        assert_eq!(o.size(), base.size());
    }
}
