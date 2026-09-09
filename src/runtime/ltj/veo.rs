use super::iterator::{LtjIterator, SpoPos};

/// The cardinalities the index reports for a variable **right now**, under
/// whatever is already bound.
///
/// This exists as a struct rather than a closure because the search calls
/// it while holding the VEO mutably: `iterators`, `var_to_iterators` and
/// `var_to_positions` are disjoint fields from `veo`, so borrowing them
/// separately is what lets `Veo::down` read the index at all.
pub struct IterSizes<'x, 'a> {
    iterators: &'x [LtjIterator<'a>],
    var_to_iterators: &'x [Vec<usize>],
    var_to_positions: &'x [Vec<SpoPos>],
}

impl<'x, 'a> IterSizes<'x, 'a> {
    pub fn new(
        iterators: &'x [LtjIterator<'a>],
        var_to_iterators: &'x [Vec<usize>],
        var_to_positions: &'x [Vec<SpoPos>],
    ) -> Self {
        IterSizes {
            iterators,
            var_to_iterators,
            var_to_positions,
        }
    }

    /// The smallest subtree any iterator holding `var` reports. The min is
    /// the reference's choice and the sound one: a variable is bounded by
    /// its most constrained occurrence, since the leapfrog intersects.
    pub fn min_size(&self, var: u8) -> usize {
        let iters = &self.var_to_iterators[var as usize];
        let positions = &self.var_to_positions[var as usize];
        let mut min = usize::MAX;
        for (k, &it) in iters.iter().enumerate() {
            let w = self.iterators[it].subtree_size(positions[k]);
            if w < min {
                min = w;
            }
        }
        min
    }
}

/// Variable Elimination Order — determines the order in which variables are bound.
///
/// Two protocols share this trait. A **materialised** order (`VeoSimple`,
/// `VeoOverride`) is decided before the search and answers `var_at(j)` for
/// any level; the four hooks below are no-ops for it. An **adaptive** order
/// (`AdaptiveVeo`) decides one variable at a time, so it only knows
/// `var_at(j)` for levels the search has already reached, and it needs the
/// hooks to track the descent.
pub trait Veo {
    /// Get the variable bound at level `j`. For an adaptive order only
    /// the levels the search has already entered have an answer.
    fn var_at(&self, j: usize) -> u8;
    /// Total number of variables.
    fn size(&self) -> usize;

    /// Pick the variable for level `j`, on entry to that level. A
    /// materialised order just reads it off; an adaptive one chooses it
    /// now and marks it bound.
    fn next(&mut self, j: usize) -> u8 {
        self.var_at(j)
    }

    /// A value was bound at the current level and the iterators have
    /// descended: re-weigh whatever that narrows.
    fn down(&mut self, _sizes: &IterSizes) {}

    /// The iterators ascended: undo what the matching `down` recorded.
    fn up(&mut self) {}

    /// The current level is exhausted: its variable goes back in the pool.
    fn done(&mut self) {}

    /// Replay a binding taken on an earlier branch: mark `var` bound at
    /// the current level, without choosing it.
    ///
    /// `memo`'s phase 2 resumes a stored prefix long after the search
    /// that produced it unwound, and it puts the *iterators* back by
    /// descending them. A materialised order needs nothing more, since
    /// `next` reads a fixed table. An adaptive one carries state — which
    /// variables are still in the pool — and would otherwise pick, below
    /// the resumed prefix, a variable the prefix already bound: the
    /// iterator then fixes a third position in a triple that has two, and
    /// the array iterator's depth assertion fires.
    ///
    /// Undone by `done`, one per `force`, exactly as `next` is.
    fn force(&mut self, _var: u8) {}

    /// Whether the order is decided during the search. Filter placement
    /// is precomputed per level for a materialised order and has to be
    /// resolved per binding for an adaptive one.
    fn is_adaptive(&self) -> bool {
        false
    }
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

    /// Move `var` to `level`, and guarantee `prereq` binds strictly
    /// before it.
    ///
    /// This is what makes a **correlated** `NEAREST` evaluable inside the
    /// join. The query vector is the anchor's, so the anchor has to be
    /// bound by the time the search variable's level is reached —
    /// otherwise there is no vector to rank against. Rather than reject
    /// an order that happens to put the anchor later, move it to just
    /// before the search variable: the two are joined anyway, so the
    /// pattern still decomposes, and the constraint is on their relative
    /// position rather than on either one's absolute level.
    ///
    /// `None` when either variable is absent from the base order (the
    /// secondary-index fold can turn one into a constant), which the
    /// caller degrades on rather than answering a different question.
    pub fn pin_at_after(base: &dyn Veo, var: u8, level: usize, prereq: u8) -> Option<VeoOverride> {
        let mut order: Vec<u8> = (0..base.size()).map(|j| base.var_at(j)).collect();
        if !order.contains(&prereq) {
            return None;
        }
        let cur = order.iter().position(|&v| v == var)?;
        let v = order.remove(cur);
        let at = level.min(order.len());
        // With `var` out of the way, is the anchor already above the slot
        // it is going into?
        let anchor_pos = order.iter().position(|&p| p == prereq)?;
        if anchor_pos < at {
            order.insert(at, v);
        } else {
            // Pull the anchor up to the slot and put the search variable
            // immediately after it.
            let a = order.remove(anchor_pos);
            order.insert(at, a);
            order.insert(at + 1, v);
        }
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

/// Whether the variable order is already decided by the pattern's shape,
/// so no weight — measured or syntactic — can change it.
///
/// True with at most one non-lonely variable (there is no pick to make)
/// and at most one lonely one (nothing to sort it against). In that case
/// `AdaptiveVeo` provably produces `VeoSimple`'s order, so the caller
/// should build the cheaper one: LDBC IC8 issues 148 323 LTJ runs of such
/// a pattern, and the two orders visit the same 600 candidates — the
/// adaptive VEO's construction was 20% of the query for an order it could
/// not change.
pub fn order_is_forced(var_info: &[(u8, usize, bool)]) -> bool {
    let lonely = var_info.iter().filter(|&&(_, _, l)| l).count();
    (var_info.len() - lonely) <= 1 && lonely <= 1
}

/// One non-lonely variable's live state in the adaptive order.
struct VarInfo {
    name: u8,
    /// Current cardinality estimate. Lowered by `down`, restored by `up`.
    weight: usize,
    /// The syntactic weight, kept as the tiebreak. The measured size is a
    /// property of the *triple*, not of the variable: with only the label
    /// fixed, both endpoints of `(a)-[:L]->(b)` report the same subtree,
    /// so it discriminates across triples and never within one. Comparing
    /// on the measured size alone therefore turns a real syntactic
    /// ranking into a tie, and the tie then falls to variable id.
    ///
    /// That is not hypothetical: LDBC IC5's outer join is a single triple
    /// `(otherPerson)<-[:hasMember]-(forum:Forum)` whose two endpoints
    /// both measure 123 268. The syntactic weights are 1 492 038 and
    /// 373 009 — `forum` carries the `:Forum` filter and is the side to
    /// bind first — and collapsing them cost 1.36× on the whole query.
    syntactic: usize,
    /// Non-lonely variables sharing a triple with this one — the only ones
    /// binding it can narrow.
    related: Vec<u8>,
    /// A variable that must be bound before this one can be picked.
    ///
    /// This is the similarity arc of a correlated `NEAREST`: the query
    /// vector is the anchor's, so the ranking does not exist until the
    /// anchor is bound. Unlike a graph edge the arc is **not symmetric** —
    /// "b is among a's k nearest" does not imply the reverse, and there is
    /// no index for the reverse direction — so it constrains the order
    /// rather than merely informing it. Without it a free order can put
    /// the search variable first, and the search then finds no anchor to
    /// rank against and quietly yields nothing.
    prereq: Option<u8>,
    is_bound: bool,
}

/// Adaptive VEO: re-pick the variable order per binding, from the subtree
/// sizes the iterators report under the current partial binding
/// (`cltj/include/veo/veo_adaptive.hpp`, issue #101).
///
/// `VeoSimple` fixes the whole order up front from a *syntactic* guess —
/// which filter a variable carries — and never revisits it. This one
/// decides one variable at a time: `next` takes the lightest unbound
/// variable, `down` re-weighs its neighbours with the cardinalities the
/// index actually holds now, and `up` restores them so backtracking is
/// exact.
///
/// Two things are kept from `VeoSimple` rather than ported from the
/// reference:
///
/// - **Lonely variables still bind last.** The argument in `VeoSimple`
///   holds unchanged: a variable in a single triple is enumerated, not
///   intersected, so elevating it trades a structural intersection for a
///   scan. The reference holds them back the same way.
/// - **The syntactic weight is kept as a ceiling** (`min` with the
///   measured size). The index knows nothing about a pushed-down
///   `x.id = K`, which really does leave one binding; dropping that would
///   make the order blind to exactly the predicates this engine works
///   hardest to push down.
///
/// Correctness does not depend on any of it — leapfrog is order-agnostic,
/// so a different order changes the cost and the row *order*, never the
/// row set.
pub struct AdaptiveVeo {
    info: Vec<VarInfo>,
    /// Lonely variables, returned after every non-lonely one.
    lonely: Vec<u8>,
    /// var id → index into `info`; `None` for a lonely or absent variable.
    pos_of: Vec<Option<usize>>,
    /// Indices into `info` not yet bound.
    not_bound: Vec<usize>,
    /// Indices into `info`, in binding order.
    bound: Vec<usize>,
    /// Per descent, the weights `down` overwrote, so `up` can put them back.
    versions: Vec<Vec<(usize, usize)>>,
    /// The variable bound at each level reached so far.
    path: Vec<u8>,
    /// Current depth: how many variables are bound.
    index: usize,
}

impl AdaptiveVeo {
    /// `var_info[i]` is `(var_id, syntactic_weight, is_lonely)` — the same
    /// input `VeoSimple` takes. `related[v]` lists the variables sharing a
    /// triple with `v`. `sizes` reads the initial cardinalities off the
    /// iterators, which at this point have descended their constants only:
    /// a labelled edge already reports a real subtree, an unlabelled one
    /// reports `usize::MAX` and leans on the syntactic weight.
    /// `prereq` is `(dependent, must_bind_first)` — see `VarInfo::prereq`.
    pub fn new(
        var_info: Vec<(u8, usize, bool)>,
        related: &[Vec<u8>],
        prereq: Option<(u8, u8)>,
        sizes: &IterSizes,
    ) -> Self {
        let num_vars = related.len();
        let mut pos_of = vec![None; num_vars];
        let mut info: Vec<VarInfo> = Vec::new();
        let mut lonely: Vec<(u8, usize, usize)> = Vec::new();

        for &(name, syntactic, is_lonely) in &var_info {
            let weight = syntactic.min(sizes.min_size(name));
            if is_lonely {
                lonely.push((name, weight, syntactic));
            } else {
                pos_of[name as usize] = Some(info.len());
                info.push(VarInfo {
                    name,
                    weight,
                    syntactic,
                    related: Vec::new(),
                    prereq: prereq.filter(|&(dep, _)| dep == name).map(|(_, on)| on),
                    is_bound: false,
                });
            }
        }

        // Only non-lonely neighbours matter: `down` re-weighs the pool it
        // picks from, and lonely variables are never in it.
        for (v, neighbours) in related.iter().enumerate() {
            let Some(pos) = pos_of.get(v).copied().flatten() else {
                continue;
            };
            for &n in neighbours {
                if pos_of.get(n as usize).copied().flatten().is_some() && n != v as u8 {
                    info[pos].related.push(n);
                }
            }
            info[pos].related.sort_unstable();
            info[pos].related.dedup();
        }

        // Lightest first among the lonely too, matching `VeoSimple`'s
        // secondary sort; the reference leaves them in pattern order.
        lonely.sort_by_key(|&(_, w, syn)| (w, syn));

        let not_bound = (0..info.len()).collect();
        AdaptiveVeo {
            info,
            lonely: lonely.into_iter().map(|(n, _, _)| n).collect(),
            pos_of,
            not_bound,
            bound: Vec::new(),
            versions: Vec::new(),
            path: Vec::new(),
            index: 0,
        }
    }
}

impl Veo for AdaptiveVeo {
    fn var_at(&self, j: usize) -> u8 {
        // Only the levels the search has already entered have an answer;
        // every caller that asks about a deeper one is a materialised-order
        // caller and is routed away by `is_adaptive`.
        debug_assert!(j < self.path.len(), "adaptive VEO has no level {} yet", j);
        self.path.get(j).copied().unwrap_or(0)
    }

    fn size(&self) -> usize {
        self.info.len() + self.lonely.len()
    }

    fn next(&mut self, _j: usize) -> u8 {
        let name = if self.index < self.info.len() {
            // Linear scan for the lightest unbound variable. The pool is
            // the query's variables, so this is a handful of comparisons.
            // A variable whose prerequisite is still unbound is not
            // eligible yet, however light it looks. There is at most one
            // such edge and its head is never itself blocked, so something
            // is always eligible; the `unwrap_or` is a belt-and-braces
            // fallback rather than a reachable path.
            let eligible = |v: &VarInfo, info: &[VarInfo]| -> bool {
                match v.prereq {
                    Some(on) => info.iter().all(|o| o.name != on || o.is_bound),
                    None => true,
                }
            };
            let mut best: Option<(usize, (usize, usize))> = None;
            for (i, &pos) in self.not_bound.iter().enumerate() {
                let v = &self.info[pos];
                if !eligible(v, &self.info) {
                    continue;
                }
                let w = (v.weight, v.syntactic);
                if best.map_or(true, |(_, bw)| w < bw) {
                    best = Some((i, w));
                }
            }
            let best = best.map(|(i, _)| i).unwrap_or(0);
            let pos = self.not_bound.remove(best);
            self.info[pos].is_bound = true;
            self.bound.push(pos);
            self.info[pos].name
        } else {
            self.lonely[self.index - self.info.len()]
        };
        self.index += 1;
        self.path.push(name);
        name
    }

    fn down(&mut self, sizes: &IterSizes) {
        // The current level is `self.index - 1`; a lonely one narrows
        // nothing that is still to come.
        if self.index > self.info.len() {
            return;
        }
        let pos_last = *self.bound.last().expect("a non-lonely level is bound");
        let related = std::mem::take(&mut self.info[pos_last].related);
        let mut version: Vec<(usize, usize)> = Vec::new();
        for &rel in &related {
            let Some(pos) = self.pos_of[rel as usize] else {
                continue;
            };
            if self.info[pos].is_bound {
                continue;
            }
            let min_w = sizes.min_size(rel);
            if min_w < self.info[pos].weight {
                version.push((pos, self.info[pos].weight));
                self.info[pos].weight = min_w;
            }
        }
        self.info[pos_last].related = related;
        self.versions.push(version);
    }

    fn up(&mut self) {
        if self.index > self.info.len() {
            return;
        }
        for (pos, w) in self.versions.pop().expect("one version per down") {
            self.info[pos].weight = w;
        }
    }

    fn force(&mut self, var: u8) {
        self.index += 1;
        self.path.push(var);
        // A lonely variable is not in `info` and `next` does not push it
        // to `bound` either, so the two stay in step and `done`'s guard
        // undoes whichever it was.
        if let Some(pos) = self.pos_of[var as usize] {
            if let Some(i) = self.not_bound.iter().position(|&p| p == pos) {
                self.not_bound.remove(i);
                self.info[pos].is_bound = true;
                self.bound.push(pos);
            }
        }
    }

    fn done(&mut self) {
        self.index -= 1;
        self.path.pop();
        if self.index < self.info.len() {
            let pos = self.bound.pop().expect("a non-lonely level is bound");
            self.info[pos].is_bound = false;
            self.not_bound.push(pos);
        }
    }

    fn is_adaptive(&self) -> bool {
        true
    }
}

#[cfg(test)]
impl AdaptiveVeo {
    /// The live weight of a non-lonely variable, so a test can watch
    /// `down` narrow it and `up` put it back.
    fn weight_of(&self, var: u8) -> Option<usize> {
        self.pos_of[var as usize].map(|p| self.info[p].weight)
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

    // ---- adaptive VEO ----

    use crate::model::graph::MemoryGraphStore;
    use crate::runtime::ltj::iterator::{Term, TriplePattern};
    use crate::runtime::ltj::triple_index::TripleIndex;

    /// A hub with many `R` edges and a single `S` edge, so the two labels
    /// have cardinalities the index can actually tell apart.
    fn skewed_graph() -> MemoryGraphStore {
        let mut nodes = String::new();
        let mut edges = String::new();
        for i in 0..12 {
            if i > 0 {
                nodes.push(',');
            }
            nodes.push_str(&format!(r#"{{"id":"n{i}","labels":["N"],"props":{{}}}}"#));
        }
        for i in 1..12 {
            if i > 1 {
                edges.push(',');
            }
            edges.push_str(&format!(
                r#"{{"id":"r{i}","labels":["R"],"props":{{}},"endpoints":["n0","n{i}"],"directionality":"->"}}"#
            ));
        }
        edges.push_str(
            r#",{"id":"s1","labels":["S"],"props":{},"endpoints":["n1","n2"],"directionality":"->"}"#,
        );
        MemoryGraphStore::from_json_str(&format!(r#"{{"nodes":[{nodes}],"edges":[{edges}]}}"#))
            .unwrap()
    }

    /// `(x)-[:R]->(y)-[:S]->(z)`: three variables, two triples.
    fn skewed_iterators(index: &TripleIndex) -> Vec<LtjIterator<'_>> {
        let r = index.label_to_id["R"];
        let s = index.label_to_id["S"];
        vec![
            LtjIterator::new(
                TriplePattern {
                    terms: [Term::Variable(0), Term::Constant(r), Term::Variable(1)],
                },
                index,
            ),
            LtjIterator::new(
                TriplePattern {
                    terms: [Term::Variable(1), Term::Constant(s), Term::Variable(2)],
                },
                index,
            ),
        ]
    }

    #[test]
    fn adaptive_picks_the_variable_the_index_says_is_smallest() {
        let g = skewed_graph();
        let index = TripleIndex::from_graph(&g);
        let iterators = skewed_iterators(&index);
        // x in triple 0 (S pos), y in both, z in triple 1 (O pos).
        let var_to_iterators = vec![vec![0], vec![0, 1], vec![1]];
        let var_to_positions = vec![vec![SpoPos::S], vec![SpoPos::O, SpoPos::S], vec![SpoPos::O]];
        let sizes = IterSizes::new(&iterators, &var_to_iterators, &var_to_positions);

        // `y` is the only non-lonely variable, so it binds first whatever
        // the weights say; what the sizes decide is x against z.
        assert_eq!(sizes.min_size(1), 1, "y is bounded by the single S edge");
        assert_eq!(sizes.min_size(0), 11, "x sees only the 11 R edges");
        assert_eq!(sizes.min_size(2), 1, "z sees only the single S edge");

        // Every variable unfiltered: the syntactic weight is the whole
        // index for all three, so the order is decided purely by the
        // measured sizes — and `z` beats `x`.
        let related = vec![vec![1], vec![0, 2], vec![1]];
        let info = vec![
            (0u8, index.len(), true),
            (1, index.len(), false),
            (2, index.len(), true),
        ];
        let mut veo = AdaptiveVeo::new(info, &related, None, &sizes);
        assert_eq!(veo.size(), 3);
        assert_eq!(veo.next(0), 1, "the only non-lonely variable binds first");
        assert_eq!(
            veo.next(1),
            2,
            "then the lonely variable the index says is smaller"
        );
        assert_eq!(veo.next(2), 0);
    }

    /// The measured size is a property of the *triple*: both endpoints of
    /// `(x)-[:R]->(y)` report the same subtree, so it cannot say which to
    /// bind first. Letting it overwrite the syntactic weight turns a real
    /// ranking into a tie — this is LDBC IC5's regression in miniature, and
    /// the syntactic tiebreak is what keeps the filtered side first.
    #[test]
    fn adaptive_breaks_a_measured_tie_on_the_syntactic_weight() {
        let g = skewed_graph();
        let index = TripleIndex::from_graph(&g);
        let r = index.label_to_id["R"];
        let iterators = vec![LtjIterator::new(
            TriplePattern {
                terms: [Term::Variable(0), Term::Constant(r), Term::Variable(1)],
            },
            &index,
        )];
        let var_to_iterators = vec![vec![0], vec![0]];
        let var_to_positions = vec![vec![SpoPos::S], vec![SpoPos::O]];
        let sizes = IterSizes::new(&iterators, &var_to_iterators, &var_to_positions);

        assert_eq!(
            sizes.min_size(0),
            sizes.min_size(1),
            "same triple, same subtree"
        );

        // Both lonely (one triple each), tied on the measurement. Variable
        // 1 carries the cheaper syntactic weight — a label filter, say —
        // so it must still bind first, and the lower variable id must not
        // win by accident.
        let related = vec![vec![1], vec![0]];
        let info = vec![(0u8, 900, true), (1, 100, true)];
        let mut veo = AdaptiveVeo::new(info, &related, None, &sizes);
        assert_eq!(veo.next(0), 1, "the syntactically cheaper side binds first");
        assert_eq!(veo.next(1), 0);
    }

    #[test]
    fn a_forced_order_is_recognised_so_the_caller_can_skip_the_adaptive_veo() {
        // One variable: no pick to make.
        assert!(order_is_forced(&[(0, 10, false)]));
        // One connector plus one lonely: lonely-last decides it.
        assert!(order_is_forced(&[(0, 10, false), (1, 20, true)]));
        // Two connectors, or two lonely, and the weights matter again.
        assert!(!order_is_forced(&[(0, 10, false), (1, 20, false)]));
        assert!(!order_is_forced(&[(0, 10, true), (1, 20, true)]));
    }

    /// A forced order must be the order `VeoSimple` would have produced,
    /// or swapping to the cheaper VEO would change results.
    #[test]
    fn a_forced_order_matches_veo_simple() {
        let g = skewed_graph();
        let index = TripleIndex::from_graph(&g);
        let iterators = skewed_iterators(&index);
        let var_to_iterators = vec![vec![0], vec![0, 1], vec![1]];
        let var_to_positions = vec![vec![SpoPos::S], vec![SpoPos::O, SpoPos::S], vec![SpoPos::O]];
        let sizes = IterSizes::new(&iterators, &var_to_iterators, &var_to_positions);
        // One connector, one lonely — the shape LDBC IC8 issues 148 323
        // times, where the adaptive VEO's construction was pure cost.
        let info = vec![(1u8, 500, false), (2, 100, true)];
        assert!(order_is_forced(&info));
        let mut adaptive = AdaptiveVeo::new(info.clone(), &vec![vec![]; 3], None, &sizes);
        let simple = VeoSimple::new(info);
        assert_eq!(
            vec![adaptive.next(0), adaptive.next(1)],
            order_of(&simple),
            "a forced order is VeoSimple's order"
        );
    }

    /// The similarity arc constrains the order, it does not merely weigh
    /// it: the anchor's vector *is* the query vector, so a search variable
    /// picked before its anchor has nothing to rank against, and the
    /// search answers such a visit with no rows at all — silently. The
    /// arc is also the one relation here that is not symmetric, so the
    /// constraint cannot be replaced by making the pair heavy.
    #[test]
    fn adaptive_never_picks_a_variable_before_its_prerequisite() {
        let g = skewed_graph();
        let index = TripleIndex::from_graph(&g);
        let iterators = skewed_iterators(&index);
        let var_to_iterators = vec![vec![0], vec![0, 1], vec![1]];
        let var_to_positions = vec![vec![SpoPos::S], vec![SpoPos::O, SpoPos::S], vec![SpoPos::O]];
        let sizes = IterSizes::new(&iterators, &var_to_iterators, &var_to_positions);
        let related = vec![vec![1, 2], vec![0, 2], vec![0, 1]];

        // Variable 0 is by far the lightest, so without the constraint it
        // binds first. Declare it the search variable of a clause anchored
        // on 2, the heaviest, and it must wait.
        let info = vec![(0u8, 1, false), (1, 500, false), (2, 900, false)];
        let mut free = AdaptiveVeo::new(info.clone(), &related, None, &sizes);
        assert_eq!(free.next(0), 0, "unconstrained, the lightest goes first");

        let mut arced = AdaptiveVeo::new(info, &related, Some((0, 2)), &sizes);
        let order = vec![arced.next(0), arced.next(1), arced.next(2)];
        let at = |v: u8| order.iter().position(|&x| x == v).expect("in the order");
        assert!(
            at(2) < at(0),
            "the anchor must bind before the search variable, got {order:?}"
        );
    }

    /// `force` is how `memo`'s phase 2 replays a prefix: the variables it
    /// names must leave the pool, or the resumed search picks one of them
    /// again and descends a triple twice.
    #[test]
    fn forcing_a_binding_takes_it_out_of_the_pool_and_done_puts_it_back() {
        let g = skewed_graph();
        let index = TripleIndex::from_graph(&g);
        let iterators = skewed_iterators(&index);
        let var_to_iterators = vec![vec![0], vec![0, 1], vec![1]];
        let var_to_positions = vec![vec![SpoPos::S], vec![SpoPos::O, SpoPos::S], vec![SpoPos::O]];
        let sizes = IterSizes::new(&iterators, &var_to_iterators, &var_to_positions);
        let related = vec![vec![1], vec![0, 2], vec![1]];
        let info = vec![(0u8, 10, false), (1, 20, false), (2, 30, false)];
        let mut veo = AdaptiveVeo::new(info, &related, None, &sizes);

        veo.force(1);
        veo.force(0);
        let rest = vec![veo.next(2)];
        assert_eq!(rest, vec![2], "only the unforced variable is left");

        veo.done();
        veo.done();
        veo.done();
        let mut back: Vec<u8> = (0..3).map(|j| veo.next(j)).collect();
        back.sort_unstable();
        assert_eq!(back, vec![0, 1, 2], "done() restores every forced binding");
    }

    #[test]
    fn adaptive_restores_weights_on_backtracking() {
        let g = skewed_graph();
        let index = TripleIndex::from_graph(&g);
        let mut iterators = skewed_iterators(&index);
        let var_to_iterators = vec![vec![0], vec![0, 1], vec![1]];
        let var_to_positions = vec![vec![SpoPos::S], vec![SpoPos::O, SpoPos::S], vec![SpoPos::O]];

        // Three non-lonely variables so `down` has an unbound neighbour to
        // re-weigh: pretend x and z also connect, which is what a comma
        // join would produce.
        let related = vec![vec![1], vec![0, 2], vec![1]];
        let info = vec![
            (0u8, usize::MAX, false),
            (1, usize::MAX, false),
            (2, usize::MAX, false),
        ];
        let mut veo = {
            let sizes = IterSizes::new(&iterators, &var_to_iterators, &var_to_positions);
            AdaptiveVeo::new(info, &related, None, &sizes)
        };

        let first = veo.next(0);
        assert_eq!(first, 1, "y is the most constrained at the root");

        // Descend the iterators the way the search does, then re-weigh.
        let before = veo.weight_of(0);
        iterators[0].down(SpoPos::O, 1);
        iterators[1].down(SpoPos::S, 1);
        {
            let sizes = IterSizes::new(&iterators, &var_to_iterators, &var_to_positions);
            veo.down(&sizes);
        }
        assert!(
            veo.weight_of(0) < before,
            "binding y narrowed x: {:?} -> {:?}",
            before,
            veo.weight_of(0)
        );

        veo.up();
        iterators[0].up(SpoPos::O);
        iterators[1].up(SpoPos::S);
        assert_eq!(
            veo.weight_of(0),
            before,
            "up() undoes exactly what down() did"
        );

        veo.done();
        let mut back: Vec<u8> = (0..3).map(|j| veo.next(j)).collect();
        back.sort_unstable();
        assert_eq!(back, vec![0, 1, 2], "done() puts y back in the pool");
    }

    #[test]
    fn override_keeps_the_size_so_filter_placement_still_lines_up() {
        let base = VeoSimple::new(vec![(0, 10, false), (1, 20, false), (2, 30, true)]);
        let o = VeoOverride::pin_at(&base, 2, 0).expect("present");
        assert_eq!(o.size(), base.size());
    }
}
