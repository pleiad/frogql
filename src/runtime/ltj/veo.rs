/// Variable Elimination Order — determines the order in which variables are bound.
pub trait Veo {
    /// Get the next variable to bind at level `j`.
    fn var_at(&self, j: usize) -> u8;
    /// Total number of variables.
    fn size(&self) -> usize;
}

/// Simple VEO: fixed order determined at construction time.
/// Sorts variables by weight (minimum children count across all iterators containing the variable).
/// Non-lonely variables (appearing in 2+ triples) come first; lonely variables last.
pub struct VeoSimple {
    order: Vec<u8>,
}

impl VeoSimple {
    /// Build a VEO from variable weights.
    /// `var_weights`: for each variable ID, (weight, is_lonely).
    /// Weight = min children count across iterators containing this variable.
    /// Lonely = appears in only one triple pattern.
    pub fn new(mut var_info: Vec<(u8, usize, bool)>) -> Self {
        // Sort: non-lonely first (ascending weight), then lonely (ascending weight)
        var_info.sort_by(|a, b| {
            a.2.cmp(&b.2) // lonely last
                .then(a.1.cmp(&b.1)) // ascending weight
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
