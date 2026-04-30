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
