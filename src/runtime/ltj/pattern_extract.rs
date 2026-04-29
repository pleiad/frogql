use std::collections::HashMap;

use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Path, PathValue};
use crate::runtime::assignment::Assignment;
use crate::runtime::result::{IntermediateResult, ResultRow};
use crate::syntax::descriptor::Descriptor;
use crate::syntax::path_pattern::PathPattern;

use super::algorithm::{FilterKind, LtjAlgorithm, LtjRunner, PlacedFilter, ResultTuple};
use super::iterator::{LtjIterator, SpoPos, Term, TriplePattern};
use super::triple_index::TripleIndex;
use super::veo::{Veo, VeoSimple};

// ---- Decomposition result ----

struct Decomposition {
    triples: Vec<TriplePattern>,
    var_id_to_name: Vec<String>,
    filters: Vec<ExtractedFilter>,
    /// Variables that are internal (anonymous) — excluded from final assignment
    internal_vars: Vec<u8>,
    /// For each triple: (src_var, tgt_var, edge_var_name, kind).
    /// `src_var` and `tgt_var` reflect the order of the *triple*, after the
    /// leftward swap; `kind` lets result reconstruction recover the
    /// original pattern direction for path display and edge lookup.
    triple_info: Vec<(u8, u8, Option<String>, EdgeKind)>,
    /// Join boundaries: triple index ranges for each sub-query in a Join.
    /// E.g., for `Q1, Q2` with 3 triples from Q1 and 2 from Q2: [(0,3), (3,5)].
    /// Empty means the whole pattern is a single path.
    join_boundaries: Vec<(usize, usize)>,
}

#[derive(Clone)]
struct ExtractedFilter {
    depends_on: Vec<u8>,
    kind: FilterKind,
}

// ---- Flat element of a concat chain ----

/// Edge direction at the pattern level. LTJ extracts a forward triple
/// `(src, label, tgt)` for every edge regardless of direction; the
/// `EdgeKind` lets the decomposition know whether to swap endpoints
/// (Left) and lets result reconstruction pick the right `PathValue`
/// variant (`EdgeUndirectional` for Undirected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeKind {
    Right,
    Left,
    Undirected,
}

#[derive(Debug)]
enum FlatElement<'a> {
    Node(Option<&'a Descriptor>),
    Edge(EdgeKind, Option<&'a Descriptor>),
}

// ---- Main entry point ----

/// Try to run LTJ on a PathPattern. Returns Some(result) if decomposable.
pub fn try_ltj<G: GraphAccess>(
    graph: &G,
    pattern: &PathPattern,
    index: &TripleIndex,
    limit: usize,
) -> Option<IntermediateResult> {
    let decomp = decompose(pattern, index)?;

    if decomp.triples.is_empty() {
        return None;
    }

    let num_vars = decomp.var_id_to_name.len();

    // Build iterators and var maps
    let mut iterators: Vec<LtjIterator> = Vec::new();
    let mut var_to_iterators: Vec<Vec<usize>> = vec![vec![]; num_vars];
    let mut var_to_positions: Vec<Vec<SpoPos>> = vec![vec![]; num_vars];

    for triple in &decomp.triples {
        let iter = LtjIterator::new(triple.clone(), index);
        let iter_idx = iterators.len();
        iterators.push(iter);

        for (pos_idx, term) in triple.terms.iter().enumerate() {
            if let Term::Variable(var_id) = term {
                let spo_pos = match pos_idx {
                    0 => SpoPos::S,
                    1 => SpoPos::P,
                    2 => SpoPos::O,
                    _ => unreachable!(),
                };
                var_to_iterators[*var_id as usize].push(iter_idx);
                var_to_positions[*var_id as usize].push(spo_pos);
            }
        }
    }

    // Build VEO: non-lonely variables first, then lonely
    let var_info: Vec<(u8, usize, bool)> = (0..num_vars)
        .map(|v| {
            let v_id = v as u8;
            let iters = &var_to_iterators[v];
            let is_lonely = iters.len() <= 1;
            let weight = if iters.is_empty() {
                usize::MAX
            } else {
                index.len()
            };
            (v_id, weight, is_lonely)
        })
        .collect();

    let veo = VeoSimple::new(var_info);

    // Place filters
    let mut filters_at_level: Vec<Vec<PlacedFilter>> = vec![vec![]; veo.size()];
    for filter in &decomp.filters {
        let max_level = filter
            .depends_on
            .iter()
            .filter_map(|&var_id| (0..veo.size()).find(|&j| veo.var_at(j) == var_id))
            .max()
            .unwrap_or(0);

        if max_level < filters_at_level.len() {
            filters_at_level[max_level].push(PlacedFilter {
                eval_at_level: max_level,
                kind: filter.kind.clone(),
            });
        }
    }

    // Run LTJ
    let algorithm = LtjAlgorithm::new(
        iterators,
        var_to_iterators,
        var_to_positions,
        Box::new(veo),
        filters_at_level,
        num_vars,
    );

    let mut runner = LtjRunner::new(algorithm, graph);
    let tuples = runner.run(limit);

    Some(convert_results(graph, &tuples, &decomp))
}

// ---- Flatten concat chains ----

/// Flatten a left-associative concat tree into a linear sequence of elements.
fn flatten_concat<'a>(p: &'a PathPattern, out: &mut Vec<FlatElement<'a>>) -> bool {
    match p {
        PathPattern::Node(d) => {
            out.push(FlatElement::Node(d.as_ref()));
            true
        }
        PathPattern::EdgeRight(d) => {
            out.push(FlatElement::Edge(EdgeKind::Right, d.as_ref()));
            true
        }
        PathPattern::EdgeLeft(d) => {
            out.push(FlatElement::Edge(EdgeKind::Left, d.as_ref()));
            true
        }
        PathPattern::EdgeUndirected(d) => {
            out.push(FlatElement::Edge(EdgeKind::Undirected, d.as_ref()));
            true
        }
        PathPattern::Concat(p1, p2) => flatten_concat(p1, out) && flatten_concat(p2, out),
        _ => false, // Not decomposable (any-direction `-[]-`, unions, repetition, …)
    }
}

// ---- Decomposition ----

fn decompose(pattern: &PathPattern, index: &TripleIndex) -> Option<Decomposition> {
    let mut triples = Vec::new();
    let mut var_name_to_id: HashMap<String, u8> = HashMap::new();
    let mut var_id_to_name: Vec<String> = Vec::new();
    let mut filters = Vec::new();
    let mut internal_vars = Vec::new();
    let mut triple_info = Vec::new();
    let mut fresh_counter = 0u32;
    let mut join_boundaries = Vec::new();

    let _last_var = decompose_pattern_top(
        pattern,
        index,
        &mut triples,
        &mut var_name_to_id,
        &mut var_id_to_name,
        &mut filters,
        &mut internal_vars,
        &mut triple_info,
        &mut fresh_counter,
        &mut join_boundaries,
    )?;

    if triples.is_empty() {
        return None;
    }

    Some(Decomposition {
        triples,
        var_id_to_name,
        filters,
        internal_vars,
        triple_info,
        join_boundaries,
    })
}

/// Top-level decomposition that tracks join boundaries.
// LTJ decomposition state — bundle of mutable accumulators that have to
// thread through the recursive walk. Refactor candidate, not a bug.
#[allow(clippy::too_many_arguments)]
fn decompose_pattern_top(
    pattern: &PathPattern,
    index: &TripleIndex,
    triples: &mut Vec<TriplePattern>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    triple_info: &mut Vec<(u8, u8, Option<String>, EdgeKind)>,
    fresh: &mut u32,
    join_boundaries: &mut Vec<(usize, usize)>,
) -> Option<u8> {
    match pattern {
        PathPattern::Join(p1, p2) => {
            let start1 = triples.len();
            decompose_pattern_top(
                p1,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
                join_boundaries,
            )?;
            // If p1 was not itself a join, record its boundary
            if join_boundaries.is_empty() || join_boundaries.last().unwrap().1 != triples.len() {
                join_boundaries.push((start1, triples.len()));
            }

            let start2 = triples.len();
            let result = decompose_pattern_top(
                p2,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
                join_boundaries,
            )?;
            // If p2 was not itself a join, record its boundary
            if join_boundaries.last().unwrap().1 != triples.len() {
                join_boundaries.push((start2, triples.len()));
            }

            Some(result)
        }
        _ => decompose_pattern(
            pattern,
            index,
            triples,
            names,
            id_to_name,
            filters,
            internal,
            triple_info,
            fresh,
        ),
    }
}

// Same accumulator bundle as decompose_pattern_top.
#[allow(clippy::too_many_arguments)]
fn decompose_pattern(
    pattern: &PathPattern,
    index: &TripleIndex,
    triples: &mut Vec<TriplePattern>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    triple_info: &mut Vec<(u8, u8, Option<String>, EdgeKind)>,
    fresh: &mut u32,
) -> Option<u8> {
    match pattern {
        PathPattern::Join(p1, p2) => {
            // Join at non-top level: decompose both sides without boundary tracking
            decompose_pattern(
                p1,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
            )?;
            decompose_pattern(
                p2,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
            )
        }

        PathPattern::Concat(_, _) => {
            // Flatten the concat chain into [Node, Edge, Node, Edge, Node, ...]
            let mut elems = Vec::new();
            if !flatten_concat(pattern, &mut elems) {
                return None;
            }
            decompose_flat_chain(
                &elems,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
            )
        }

        PathPattern::Node(desc) => {
            // Standalone node — no triple, just return the var
            Some(node_var(
                desc.as_ref(),
                names,
                id_to_name,
                filters,
                internal,
                fresh,
            ))
        }

        PathPattern::Filter(inner, _expr) => decompose_pattern(
            inner,
            index,
            triples,
            names,
            id_to_name,
            filters,
            internal,
            triple_info,
            fresh,
        ),

        _ => None,
    }
}

/// Decompose a flat chain [Node, Edge, Node, Edge, Node] into triples.
// Same accumulator bundle as decompose_pattern_top.
#[allow(clippy::too_many_arguments)]
fn decompose_flat_chain(
    elems: &[FlatElement],
    index: &TripleIndex,
    triples: &mut Vec<TriplePattern>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    triple_info: &mut Vec<(u8, u8, Option<String>, EdgeKind)>,
    fresh: &mut u32,
) -> Option<u8> {
    // Expected pattern: Node (Edge Node)*
    // Minimum 3 elements for one triple: Node Edge Node
    if elems.is_empty() {
        return None;
    }

    // First element must be a Node
    let FlatElement::Node(first_desc) = &elems[0] else {
        return None;
    };
    let mut current_node = node_var(*first_desc, names, id_to_name, filters, internal, fresh);

    let mut i = 1;
    while i + 1 < elems.len() {
        // Expect: Edge, Node
        let FlatElement::Edge(kind, edge_desc) = &elems[i] else {
            return None;
        };
        let FlatElement::Node(tgt_desc) = &elems[i + 1] else {
            return None;
        };

        let next_var = node_var(*tgt_desc, names, id_to_name, filters, internal, fresh);
        let edge_var_name = edge_desc.and_then(|d| d.var.clone());
        let p_term = build_p_term(*edge_desc, index, names, id_to_name, internal, fresh);

        // Forward (`-[L]->`) and undirected (`~[L]~`) emit a triple
        // (current, L, next). Leftward (`<-[L]-`) emits (next, L, current)
        // — i.e. the physical directed edge that flows from `next` into
        // `current`. Undirected schema entries are present in both senses
        // in the index, so a forward triple lookup catches them.
        let (src_var, tgt_var) = match kind {
            EdgeKind::Right | EdgeKind::Undirected => (current_node, next_var),
            EdgeKind::Left => (next_var, current_node),
        };

        triples.push(TriplePattern {
            terms: [Term::Variable(src_var), p_term, Term::Variable(tgt_var)],
        });
        triple_info.push((src_var, tgt_var, edge_var_name, *kind));

        current_node = next_var;
        i += 2;
    }

    // If there's a trailing edge with no node after it, create a fresh target
    if i < elems.len() {
        if let FlatElement::Edge(kind, edge_desc) = &elems[i] {
            let next_var = fresh_var(names, id_to_name, internal, fresh);
            let edge_var_name = edge_desc.and_then(|d| d.var.clone());
            let p_term = build_p_term(*edge_desc, index, names, id_to_name, internal, fresh);
            let (src_var, tgt_var) = match kind {
                EdgeKind::Right | EdgeKind::Undirected => (current_node, next_var),
                EdgeKind::Left => (next_var, current_node),
            };

            triples.push(TriplePattern {
                terms: [Term::Variable(src_var), p_term, Term::Variable(tgt_var)],
            });
            triple_info.push((src_var, tgt_var, edge_var_name, *kind));
            current_node = next_var;
        }
    }

    if triples.is_empty() {
        return None; // Just a node, no edges
    }

    Some(current_node)
}

// ---- Helper functions ----

fn node_var(
    desc: Option<&Descriptor>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    fresh: &mut u32,
) -> u8 {
    let var_id = if let Some(d) = desc {
        if let Some(name) = &d.var {
            get_or_create(name, names, id_to_name)
        } else {
            fresh_var(names, id_to_name, internal, fresh)
        }
    } else {
        fresh_var(names, id_to_name, internal, fresh)
    };

    // Extract label filters
    if let Some(d) = desc {
        for label in d.dtype.label.required_labels() {
            filters.push(ExtractedFilter {
                depends_on: vec![var_id],
                kind: FilterKind::NodeLabel {
                    var_id,
                    label: label.to_string(),
                },
            });
        }
    }

    var_id
}

fn get_or_create(name: &str, names: &mut HashMap<String, u8>, id_to_name: &mut Vec<String>) -> u8 {
    if let Some(&id) = names.get(name) {
        id
    } else {
        let id = id_to_name.len() as u8;
        id_to_name.push(name.to_string());
        names.insert(name.to_string(), id);
        id
    }
}

fn fresh_var(
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    internal: &mut Vec<u8>,
    counter: &mut u32,
) -> u8 {
    let name = format!("_ltj_{}", counter);
    *counter += 1;
    let id = get_or_create(&name, names, id_to_name);
    internal.push(id);
    id
}

/// Build the P-term of a triple pattern from an edge descriptor: a label
/// constant when there's exactly one required label, a fresh variable
/// otherwise (wildcard / multi-label).
fn build_p_term(
    edge_desc: Option<&Descriptor>,
    index: &TripleIndex,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    internal: &mut Vec<u8>,
    fresh: &mut u32,
) -> Term {
    if let Some(d) = edge_desc {
        let labels = d.dtype.label.required_labels();
        if labels.len() == 1 {
            return match index.label_to_id.get(labels[0]) {
                Some(&lid) => Term::Constant(lid),
                None => Term::Constant(u32::MAX), // label not in graph
            };
        }
    }
    let p_var = fresh_var(names, id_to_name, internal, fresh);
    Term::Variable(p_var)
}

// ---- Result conversion ----

fn convert_results<G: GraphAccess>(
    graph: &G,
    tuples: &[ResultTuple],
    decomp: &Decomposition,
) -> IntermediateResult {
    let mut rows = Vec::new();

    // Determine which triple ranges correspond to separate paths.
    // If join_boundaries is non-empty, each boundary produces a separate path.
    // Otherwise, all triples form a single path.
    let ranges: Vec<(usize, usize)> = if decomp.join_boundaries.is_empty() {
        vec![(0, decomp.triple_info.len())]
    } else {
        decomp.join_boundaries.clone()
    };

    for tuple in tuples {
        let mut assignment = Assignment::new();

        // Build assignment, excluding internal variables
        for &(var_id, value) in tuple {
            if decomp.internal_vars.contains(&var_id) {
                continue;
            }
            let name = &decomp.var_id_to_name[var_id as usize];
            assignment.extend(name.clone(), PathValue::Node(value));
        }

        // Build one path per range
        let mut paths = Vec::new();
        for &(start, end) in &ranges {
            let mut path_elements = Vec::new();
            for ti in start..end {
                let (src_var, tgt_var, ref edge_var, _kind) = decomp.triple_info[ti];
                let src_id = tuple.iter().find(|(v, _)| *v == src_var).map(|(_, id)| *id);
                let tgt_id = tuple.iter().find(|(v, _)| *v == tgt_var).map(|(_, id)| *id);

                if let (Some(src), Some(tgt)) = (src_id, tgt_id) {
                    if path_elements.is_empty() {
                        path_elements.push(PathValue::Node(src));
                    }
                    if let Some(eid) = find_edge(graph, src, tgt) {
                        // Use edge_path_value so undirected edges land as
                        // PathValue::EdgeUndirectional rather than being
                        // forced into the directional variant.
                        let pv = graph.edge_path_value(eid);
                        path_elements.push(pv.clone());
                        if let Some(ref ev) = edge_var {
                            assignment.extend(ev.clone(), pv);
                        }
                    }
                    path_elements.push(PathValue::Node(tgt));
                }
            }
            paths.push(Path(path_elements));
        }

        rows.push(ResultRow::with_paths(paths, assignment));
    }

    IntermediateResult::new(rows)
}

/// Resolve the edge id for a (src, tgt) pair. Walks outgoing-directional
/// adjacency first; falls back to undirected adjacency when the matching
/// triple was sourced from an undirected edge (those entries are emitted
/// in both senses by `TripleIndex::from_graph`, so either endpoint may
/// appear as the triple's "source").
fn find_edge<G: GraphAccess>(graph: &G, src: Id, tgt: Id) -> Option<Id> {
    if let Some(&eid) = graph
        .outgoing_edges(src)
        .iter()
        .find(|&&eid| graph.tgt(eid) == tgt)
    {
        return Some(eid);
    }
    graph.undirected_edges_of(src).iter().copied().find(|&eid| {
        let s = graph.src(eid);
        let t = graph.tgt(eid);
        (s == src && t == tgt) || (s == tgt && t == src)
    })
}
