//! ISO/IEC 39075:2024 §13 data-modification execution.
//!
//! `Runtime::run_dm` dispatches to `apply_insert` / `apply_delete` per
//! binding produced by the MATCH chain, with all atomicity / endpoint
//! validation handled here. The `&dyn GraphAccessMut` argument lets the
//! same entry point work over both `LazyGraphStore` (production backend)
//! and any future store that implements the trait.
//!
//! MVP-0 surface (see plan §"Fase 3"):
//!   - INSERT (standalone or after MATCH chain) with literal property
//!     values; bound vs defining IEP resolution per §16.5 GR8 / GR10.
//!   - DELETE / NODETACH DELETE / DETACH DELETE on bare variable refs.
//!   - Optional RETURN that projects post-mutation rows.
//!
//! Skipped here, lands in dedicated phases:
//!   - G2000 graph-type validation (Fase 6).
//!   - DEFAULT dirty-flag refresh (Fase 7).
//!   - SET / REMOVE (MVP-1).

use std::collections::HashMap;

use crate::model::graph::Props;
use crate::model::graph_access::{GraphAccess, GraphAccessMut, G1001};
use crate::model::value::{Id, PathValue};
use crate::runtime::assignment::Assignment;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::dm::{DmOp, DmStatement};
use crate::syntax::expr::Expr;
use crate::syntax::path_pattern::PathPattern;
use crate::typing::label_type::LabelType;

/// Result of executing a DML statement.
///
/// The runtime returns the same `QueryResult` shape for both DM and
/// queries because the REPL / Python bindings expect a uniform output
/// type. The `Vec<Assignment>` here is the post-mutation working table
/// (with new bindings from INSERT). When the statement carries a RETURN
/// it gets projected the same way `Runtime::run_query` projects regular
/// queries; when there is no RETURN the table is returned as-is.
#[derive(Debug)]
pub struct DmExecution {
    /// Working table after applying the DML, one row per surviving binding.
    pub rows: Vec<Assignment>,
    /// Counts for diagnostics — printed by the REPL after a DML completes.
    pub nodes_inserted: usize,
    pub edges_inserted: usize,
    pub nodes_deleted: usize,
    pub edges_deleted: usize,
}

/// Execute one DML statement against a mutable store.
///
/// `validation_schema` lets the caller plug in ISO §13 G2000 validation:
/// when `Some`, every freshly inserted node and edge is checked against
/// the schema, and the whole statement aborts atomically on the first
/// mismatch. The REPL / Python bindings pass the active GRAPH TYPE here
/// only when it isn't DEFAULT (DEFAULT is data-derived and gets refreshed
/// lazily — see Fase 7).
///
/// Currently a free function because the MATCH-chain phase needs an
/// immutable `Runtime<&G>` while the mutation phase needs the same `&G`
/// reused as `&dyn GraphAccessMut`. Wrapping it as a method on `Runtime`
/// would force the trait bound on the runtime's generic parameter and
/// ripple through every existing call site. Free function is the
/// minimum-disturbance shape.
pub fn run_dm<G>(
    graph: &G,
    dm: &DmStatement,
    validation_schema: Option<&crate::typing::variable_type::Schema>,
) -> Result<DmExecution, String>
where
    G: GraphAccess + GraphAccessMut,
{
    // 1. Resolve the MATCH chain (read-only). Standalone INSERT runs once
    // with a single empty assignment.
    let bindings: Vec<Assignment> = if dm.matches.is_empty() {
        vec![Assignment::new()]
    } else {
        let q = crate::syntax::query::Query {
            matches: dm.matches.clone(),
            group_by: None,
            returns: None,
            distinct: false,
            order_by: None,
            limit: None,
        };
        // Run the match chain through the same elaborate-then-execute path
        // that compile_query uses, so value-equality filters from the
        // pattern (`{name: 'Alice'}`) lower into `WHERE` conjuncts and
        // actually filter rows. Without this the Descriptor's
        // `value_filters` field would be silently ignored at runtime.
        let elaborated = crate::elaborate::elaborate_query(q);
        let runtime = crate::runtime::engine::Runtime::new(graph);
        let ir = runtime.run(&elaborated.collapsed_pattern());
        ir.rows.into_iter().map(|r| r.assignment).collect()
    };

    let mut exec = DmExecution {
        rows: Vec::with_capacity(bindings.len()),
        nodes_inserted: 0,
        edges_inserted: 0,
        nodes_deleted: 0,
        edges_deleted: 0,
    };

    // 2. Per-binding apply. The rollback hook lives on the store; on
    // any failure we ask it to wipe the overlay (atomicity, §13 Note 196).
    let result: Result<(), String> = (|| {
        for mu in &bindings {
            match &dm.op {
                DmOp::Insert(patterns) => {
                    let mut new_mu = mu.clone();
                    for pattern in patterns {
                        let stats =
                            apply_insert_pattern(graph, pattern, &mut new_mu, validation_schema)?;
                        exec.nodes_inserted += stats.nodes;
                        exec.edges_inserted += stats.edges;
                    }
                    exec.rows.push(new_mu);
                }
                DmOp::Delete { detach, targets } => {
                    let stats = apply_delete(graph, *detach, targets, mu)?;
                    exec.nodes_deleted += stats.nodes;
                    exec.edges_deleted += stats.edges;
                    exec.rows.push(mu.clone());
                }
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        // Atomicity per ISO §13.5 Note 196: any DML that fails must leave
        // the graph unchanged. For MVP-0 the simplest correct policy is to
        // discard the whole session overlay; this loses earlier successful
        // statements in the same REPL session, which we accept because
        // there is no transaction boundary smaller than the connection
        // until WAL lands.
        graph.rollback_session();
        return Err(e);
    }

    Ok(exec)
}

// ---- helpers ---------------------------------------------------------------

struct InsertStats {
    nodes: usize,
    edges: usize,
}

struct DeleteStats {
    nodes: usize,
    edges: usize,
}

/// Walk one `<insert path pattern>` for one binding row, allocating
/// nodes / edges as needed. Mutates `mu` so newly-defined variables are
/// visible to a later RETURN clause.
fn apply_insert_pattern<G>(
    graph: &G,
    pattern: &PathPattern,
    mu: &mut Assignment,
    schema: Option<&crate::typing::variable_type::Schema>,
) -> Result<InsertStats, String>
where
    G: GraphAccess + GraphAccessMut,
{
    let elements = flatten_insert_pattern(pattern)?;
    let mut stats = InsertStats { nodes: 0, edges: 0 };

    // Pass 1: resolve all node ids (allocate when defining, look up when
    // bound). Edge endpoints reference the result of this pass, so do it
    // up-front rather than interleaved.
    let mut node_ids: Vec<Id> = Vec::new();
    for el in &elements {
        if let FlatEl::Node(d) = el {
            node_ids.push(resolve_or_insert_node(
                graph,
                d.as_ref(),
                mu,
                &mut stats,
                schema,
            )?);
        }
    }

    // Pass 2: emit edges. The flat list alternates Node, Edge, Node, ...
    // so the n-th edge sits between node_ids[n] and node_ids[n+1].
    let mut node_cursor = 0usize;
    for el in &elements {
        match el {
            FlatEl::Node(_) => {
                node_cursor += 1;
            }
            FlatEl::Edge { dir, descriptor } => {
                let left = node_ids[node_cursor - 1];
                let right = node_ids[node_cursor];
                insert_edge_now(
                    graph,
                    *dir,
                    descriptor.as_ref(),
                    left,
                    right,
                    mu,
                    &mut stats,
                    schema,
                )?;
            }
        }
    }

    Ok(stats)
}

#[derive(Debug, Clone)]
enum FlatEl {
    Node(Option<Descriptor>),
    Edge {
        dir: EdgeDir,
        descriptor: Option<Descriptor>,
    },
}

#[derive(Debug, Clone, Copy)]
enum EdgeDir {
    Right,
    Left,
    Undirected,
}

fn flatten_insert_pattern(p: &PathPattern) -> Result<Vec<FlatEl>, String> {
    let mut out = Vec::new();
    flatten_into(p, &mut out)?;
    // Quick sanity: must alternate Node, Edge, Node, ..., starting and
    // ending with Node (every <insert path pattern> per ISO §16.5).
    if out.is_empty() {
        return Err("empty INSERT path pattern".into());
    }
    if !matches!(out.first(), Some(FlatEl::Node(_))) || !matches!(out.last(), Some(FlatEl::Node(_)))
    {
        return Err("INSERT path pattern must start and end with a node".into());
    }
    Ok(out)
}

fn flatten_into(p: &PathPattern, out: &mut Vec<FlatEl>) -> Result<(), String> {
    match p {
        PathPattern::Node(d) => {
            out.push(FlatEl::Node(d.clone()));
            Ok(())
        }
        PathPattern::EdgeRight(d) => {
            out.push(FlatEl::Edge {
                dir: EdgeDir::Right,
                descriptor: d.clone(),
            });
            Ok(())
        }
        PathPattern::EdgeLeft(d) => {
            out.push(FlatEl::Edge {
                dir: EdgeDir::Left,
                descriptor: d.clone(),
            });
            Ok(())
        }
        PathPattern::EdgeUndirected(d) => {
            out.push(FlatEl::Edge {
                dir: EdgeDir::Undirected,
                descriptor: d.clone(),
            });
            Ok(())
        }
        PathPattern::Concat(a, b) => {
            flatten_into(a, out)?;
            flatten_into(b, out)
        }
        // The DM AST validator already rejected these at parse time
        // (`validate_insert_pattern` in src/syntax/dm.rs). Reaching them
        // here is a logic error somewhere upstream.
        other => Err(format!(
            "INSERT path pattern contains an unsupported construct: {other:?}"
        )),
    }
}

fn resolve_or_insert_node<G>(
    graph: &G,
    desc: Option<&Descriptor>,
    mu: &mut Assignment,
    stats: &mut InsertStats,
    schema: Option<&crate::typing::variable_type::Schema>,
) -> Result<Id, String>
where
    G: GraphAccess + GraphAccessMut,
{
    // Bound case (§16.5 GR8): the variable name is already in the binding
    // table; reuse its id and reject any label/property set on the IEP.
    if let Some(name) = desc.and_then(|d| d.var.as_deref()) {
        if let Some(pv) = mu.get(name) {
            if let Some(d) = desc {
                if !is_label_empty(&d.dtype.label) || !d.value_filters.is_empty() {
                    return Err(format!(
                        "INSERT: variable {name} is already bound; bound IEPs cannot \
                         carry labels or property sets (§16.5 GR10 a.iii)"
                    ));
                }
            }
            return pv
                .id()
                .ok_or_else(|| format!("INSERT: variable {name} is not a node reference"));
        }
    }

    // Defining case: allocate a brand-new node.
    let labels = desc
        .map(|d| labels_from_dtype(&d.dtype.label))
        .transpose()?
        .unwrap_or(LabelType::Star);
    let props = desc
        .map(literal_props_from_descriptor)
        .transpose()?
        .unwrap_or_default();
    // ISO §13.2 GR7: validate post-insert against the active GRAPH TYPE
    // before the schedule is committed. We do it pre-insert here because
    // the descriptor we have in hand exactly describes what the node
    // would look like after `insert_node` returns.
    if let Some(schema) = schema {
        crate::typing::validate::validate_node_against_schema(&labels, &props, schema)?;
    }
    let id = graph.insert_node(labels, props);
    stats.nodes += 1;
    if let Some(name) = desc.and_then(|d| d.var.as_deref()) {
        mu.extend(name.to_string(), PathValue::Node(id));
    }
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
fn insert_edge_now<G>(
    graph: &G,
    dir: EdgeDir,
    desc: Option<&Descriptor>,
    left: Id,
    right: Id,
    mu: &mut Assignment,
    stats: &mut InsertStats,
    schema: Option<&crate::typing::variable_type::Schema>,
) -> Result<Id, String>
where
    G: GraphAccess + GraphAccessMut,
{
    // Bound edge IEPs aren't allowed by §16.5 GR10 a (insert edge patterns
    // must always be defining). The parser keeps the same Descriptor shape
    // as MATCH edges, so we only check the variable rebind issue here.
    if let Some(d) = desc {
        if let Some(name) = &d.var {
            if mu.get(name).is_some() {
                return Err(format!(
                    "INSERT: edge variable {name} is already bound; insert edge patterns \
                     are always defining (§16.5 GR10 a.i)"
                ));
            }
        }
    }
    // ISO §13.2 GR8/GR9: endpoints must exist in CG and not be deleted.
    if !graph.is_node_alive(left) {
        return Err(format!(
            "INSERT edge: endpoint node id {left} is not in the current working graph (G1003)"
        ));
    }
    if !graph.is_node_alive(right) {
        return Err(format!(
            "INSERT edge: endpoint node id {right} is not in the current working graph (G1003)"
        ));
    }

    let labels = desc
        .map(|d| labels_from_dtype(&d.dtype.label))
        .transpose()?
        .unwrap_or(LabelType::Star);
    let props = desc
        .map(literal_props_from_descriptor)
        .transpose()?
        .unwrap_or_default();

    let (src, tgt, directed) = match dir {
        EdgeDir::Right => (left, right, true),
        EdgeDir::Left => (right, left, true),
        EdgeDir::Undirected => (left, right, false),
    };
    let id = graph.insert_edge(src, tgt, directed, labels, props);
    stats.edges += 1;
    // ISO §13.2 GR7 G2000 check. Done after the insert because the
    // schema check needs to read the edge with its endpoints.
    if let Some(schema) = schema {
        crate::typing::validate::validate_edge_against_schema(graph, id, schema)?;
    }
    if let Some(name) = desc.and_then(|d| d.var.as_deref()) {
        let pv = if directed {
            PathValue::EdgeDirectional(id)
        } else {
            PathValue::EdgeUndirectional(id)
        };
        mu.extend(name.to_string(), pv);
    }
    Ok(id)
}

fn labels_from_dtype(lt: &LabelType) -> Result<LabelType, String> {
    match lt {
        LabelType::Star | LabelType::Top => Ok(LabelType::Star),
        LabelType::Label(_) | LabelType::And(_, _) => Ok(lt.clone()),
        // `(:A | :B)` and `(:!L)` are valid in MATCH but ambiguous in
        // INSERT — they describe a set of acceptable shapes rather than
        // a concrete element to create.
        LabelType::Or(_, _) => Err(
            "INSERT: label union (A | B) is not allowed in element creation; pick one label".into(),
        ),
        LabelType::Neg(_) => {
            Err("INSERT: negated labels (!:L) are not allowed in element creation".into())
        }
        LabelType::Empty => Err("INSERT: empty label set rejected".into()),
    }
}

fn is_label_empty(lt: &LabelType) -> bool {
    matches!(lt, LabelType::Star)
}

fn literal_props_from_descriptor(desc: &Descriptor) -> Result<Props, String> {
    // INSERT property values are literals in MVP-0. Anything more complex
    // (var.attr, function calls, etc.) requires a binding-aware evaluator
    // which lands when MVP-1 ships SET/REMOVE.
    let mut out: Props = HashMap::new();
    for (name, expr) in &desc.value_filters {
        match expr {
            Expr::Const(v) => {
                out.insert(name.clone(), v.clone());
            }
            other => {
                return Err(format!(
                    "INSERT: only literal property values are supported in MVP-0; \
                     property '{name}' uses {other}"
                ));
            }
        }
    }
    Ok(out)
}

fn apply_delete<G>(
    graph: &G,
    detach: bool,
    targets: &[String],
    mu: &Assignment,
) -> Result<DeleteStats, String>
where
    G: GraphAccess + GraphAccessMut,
{
    let mut stats = DeleteStats { nodes: 0, edges: 0 };
    for var in targets {
        let Some(pv) = mu.get(var) else {
            return Err(format!(
                "DELETE: variable {var} is not bound — every DELETE target needs a \
                 preceding MATCH that produces it"
            ));
        };
        match pv {
            PathValue::Node(id) => {
                let id = *id;
                if detach {
                    // detach_delete_node accumulates counts by hand.
                    let incident: Vec<Id> = graph
                        .outgoing_edges(id)
                        .into_iter()
                        .chain(graph.incoming_edges(id))
                        .chain(graph.undirected_edges_of(id))
                        .collect();
                    graph.detach_delete_node(id);
                    stats.nodes += 1;
                    stats.edges += incident.len();
                } else {
                    match graph.delete_node_no_detach(id) {
                        Ok(()) => {
                            stats.nodes += 1;
                        }
                        Err(G1001 {
                            node,
                            remaining_edges,
                        }) => {
                            return Err(format!(
                                "DELETE (NODETACH) on node id {node}: G1001 dependent \
                                 object error — {} edges still exist; use DETACH DELETE",
                                remaining_edges.len()
                            ));
                        }
                    }
                }
            }
            PathValue::EdgeDirectional(id) | PathValue::EdgeUndirectional(id) => {
                graph.delete_edge(*id);
                stats.edges += 1;
            }
            PathValue::Nothing | PathValue::Group(_) => {
                return Err(format!(
                    "DELETE: variable {var} is not a node or edge reference"
                ));
            }
        }
    }
    Ok(stats)
}
