//! Typechecker for gqlite path patterns and queries.
//!
//! Ported from `fppc/src/typechecker/checker.rs`. Public surface mirrors
//! fppc's `Typechecker` / `TypecheckResult`; the differences are documented
//! in `docs/typechecker_migration.md`.

use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{Aggregator, MatchStatement, Query, ReturnItem};

use super::descriptor_type::DescriptorType;
use super::label_type::LabelType;
use super::path_type::{EdgeDir, PathType};
use super::property_type::PropertyType;
use super::simple_type::SimpleType;
use super::type_environment::TypeEnvironment;
use super::variable_type::{Schema, VariableType};

/// Result of type-checking a path pattern or query.
#[derive(Clone, Debug)]
pub struct TypecheckResult {
    pub path: PathType,
    pub env: TypeEnvironment,
    pub ok: bool,
    pub empty: bool,
}

impl TypecheckResult {
    fn new(path: PathType, env: TypeEnvironment) -> Self {
        TypecheckResult {
            path,
            env,
            ok: true,
            empty: false,
        }
    }
}

/// The typechecker. Owns a `Schema` and accumulates errors / warnings
/// across one `check_*` invocation.
pub struct Typechecker {
    pub schema: Schema,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Typechecker {
    pub fn new(schema: Schema) -> Self {
        Typechecker {
            schema,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Permissive checker — `Schema::star()`.
    pub fn untyped() -> Self {
        Typechecker::new(Schema::star())
    }

    /// Type-check a full Query. The single public entry point: clears
    /// the diagnostic buckets, walks the pattern, then the GROUP BY and
    /// RETURN clauses, then rolls up `ok` from the accumulated errors.
    ///
    /// Result-row typing is still out of scope. Callers that only have a
    /// `PathPattern` should wrap it via `Query::pattern_only(p)` and
    /// invoke this — see `compile()` in `src/lib.rs`.
    pub fn check_query(&mut self, q: &Query) -> TypecheckResult {
        self.errors.clear();
        self.warnings.clear();

        let mut r = if q.has_any_optional() {
            self.check_match_chain(&q.matches)
        } else {
            self.check_path_pattern(&q.collapsed_pattern())
        };
        r.empty = r.path.is_unsatisfiable() || r.env.is_empty();

        if let Some(group_by) = &q.group_by {
            self.check_group_by(group_by, &r.env);
        }
        if let Some(returns) = &q.returns {
            self.check_returns(returns, &r.env);
            match &q.group_by {
                Some(group_by) => self.check_returns_match_group_by(returns, group_by),
                None => self.check_no_implicit_group_by(returns),
            }
        }

        if !self.errors.is_empty() {
            r.ok = false;
        }
        r
    }

    /// Sequential walk of the match chain when at least one is OPTIONAL.
    /// Implements TSeq composed with TMatch / TOpt: each Simple meets, each
    /// Optional outer-joins. The resulting `PathType` is the meet of the
    /// individual patterns (the path-shape part is unaffected by OPTIONAL —
    /// only the env tracks which variables can be Null).
    fn check_match_chain(&mut self, matches: &[MatchStatement]) -> TypecheckResult {
        let mut iter = matches.iter();
        let first = iter
            .next()
            .expect("Query::matches must contain at least one match statement");
        let first_r = self.check_path_pattern(first.pattern());
        // The first statement's environment is what we accumulate from. For
        // a leading OPTIONAL, every variable it introduces gains Null per
        // TOpt with Γ₁ = ∅.
        let (mut env, mut path) = if first.is_optional() {
            let acc = TypeEnvironment::outer_join(
                &self.schema,
                &TypeEnvironment::new(),
                &first_r.env,
            );
            (acc, first_r.path)
        } else {
            (first_r.env, first_r.path)
        };

        for m in iter {
            let r = self.check_path_pattern(m.pattern());
            env = match m {
                MatchStatement::Simple { .. } => match TypeEnvironment::meet(
                    &self.schema, &env, &r.env,
                ) {
                    Ok(e) => {
                        self.warn_for_collapsed_bindings(&e, &env, &r.env);
                        e
                    }
                    Err(e) => {
                        self.errors
                            .push(format!("Concatenation of contexts failed: {}", e));
                        env
                    }
                },
                MatchStatement::Optional { .. } => {
                    TypeEnvironment::outer_join(&self.schema, &env, &r.env)
                }
            };
            path = PathType::meet(&self.schema, &path, &r.path);
        }

        TypecheckResult::new(path, env)
    }

    /// ISO §16.15: mixing aggregates with non-aggregate items requires
    /// an explicit GROUP BY. Implicit Cypher-style grouping was dropped
    /// because patterns like `RETURN x.name, COUNT(*)` silently gave
    /// useless per-row counts. `compile_query_unchecked` bypasses.
    fn check_no_implicit_group_by(&mut self, items: &[ReturnItem]) {
        let has_agg = items.iter().any(|i| i.is_aggregate());
        let has_expr = items.iter().any(|i| !i.is_aggregate());
        if has_agg && has_expr {
            self.errors.push(
                "RETURN mixes aggregate and non-aggregate items but no GROUP BY \
                 clause is present. Add `GROUP BY <expr>...` before the RETURN."
                    .to_string(),
            );
        }
    }

    fn check_group_by(&mut self, exprs: &[Expr], env: &TypeEnvironment) {
        for e in exprs {
            let _ = self.check_expr(e, env);
        }
    }

    /// ISO §16.15: every non-aggregate RETURN item must structurally
    /// match a grouping expression. `Expr::eq` is structural — `x.a + 1`
    /// in RETURN with `x.a` in GROUP BY is rejected.
    fn check_returns_match_group_by(&mut self, items: &[ReturnItem], group_by: &[Expr]) {
        for item in items {
            if let ReturnItem::Expr { expr, .. } = item {
                if !group_by.iter().any(|g| g == expr) {
                    self.errors.push(format!(
                        "RETURN item `{expr}` is not in the GROUP BY clause; \
                         non-aggregate projections must match a grouping key."
                    ));
                }
            }
        }
    }

    /// Walks each item's inner expr against the env; result types are
    /// discarded — only errors/warnings (e.g. unbound vars) are collected.
    fn check_returns(&mut self, items: &[ReturnItem], env: &TypeEnvironment) {
        for item in items {
            match item {
                ReturnItem::Expr { expr, .. } => {
                    let _ = self.check_expr(expr, env);
                }
                ReturnItem::Aggregate { agg, .. } => match agg {
                    Aggregator::CountStar => {}
                    Aggregator::GeneralSet { expr, .. } => {
                        let _ = self.check_expr(expr, env);
                    }
                },
            }
        }
    }

    // -----------------------------------------------
    // Path pattern checking
    // -----------------------------------------------

    fn check_path_pattern(&mut self, node: &PathPattern) -> TypecheckResult {
        match node {
            PathPattern::Node(desc) => {
                let t = self.refine_pattern_node(desc);
                let p = PathType::from_variable(&t, EdgeDir::Any);
                let env = create_context(desc, t);
                TypecheckResult::new(p, env)
            }

            PathPattern::EdgeRight(desc) => self.check_edge(EdgeDir::Right, desc),
            PathPattern::EdgeLeft(desc) => self.check_edge(EdgeDir::Left, desc),
            PathPattern::EdgeUndirected(desc) => self.check_edge(EdgeDir::None, desc),
            PathPattern::EdgeAnyDirection(desc) => self.check_edge(EdgeDir::Any, desc),

            PathPattern::Concat(p1, p2) | PathPattern::Join(p1, p2) => {
                // Concat composes two patterns over a shared endpoint variable.
                // Join is gqlite's comma-join — patterns sharing variables but
                // not endpoint-glued. For typing both reduce to: meet the
                // environments under the schema and meet the path shapes.
                // The shape distinction matters at runtime, not for types.
                let r1 = self.check_path_pattern(p1);
                let r2 = self.check_path_pattern(p2);

                let cm = match TypeEnvironment::meet(&self.schema, &r1.env, &r2.env) {
                    Ok(env) => env,
                    Err(e) => {
                        self.errors
                            .push(format!("Concatenation of contexts failed: {}", e));
                        r1.env.clone()
                    }
                };

                self.warn_for_collapsed_bindings(&cm, &r1.env, &r2.env);

                let p = PathType::meet(&self.schema, &r1.path, &r2.path);
                TypecheckResult::new(p, cm)
            }

            PathPattern::Filter(pattern, expr) => {
                let r = self.check_path_pattern(pattern);
                let t = self.check_expr(expr, &r.env);

                if SimpleType::meet(&t, &SimpleType::B).is_empty() {
                    self.warnings.push(format!(
                        "Filter expression has type {}, which is not a boolean",
                        t
                    ));
                    TypecheckResult::new(PathType::Zero, r.env)
                } else {
                    r
                }
            }

            PathPattern::Union(p1, p2) => {
                let r1 = self.check_path_pattern(p1);
                let r2 = self.check_path_pattern(p2);
                TypecheckResult::new(
                    PathType::union(r1.path, r2.path),
                    TypeEnvironment::union(&r1.env, &r2.env),
                )
            }

            PathPattern::Repeat {
                pattern,
                lb,
                ub: _ub,
            } => {
                let r = self.check_path_pattern(pattern);
                let raw_lb = *lb as u64;

                let effective_lb = if !r.path.is_empty() {
                    raw_lb.min(3)
                } else {
                    self.warnings
                        .push("Repeat expression must have length > 0".to_string());
                    raw_lb
                };

                TypecheckResult::new(self.pow_path_type(&r.path, effective_lb), r.env.to_group())
            }

            PathPattern::Questioned(p) => {
                let r = self.check_path_pattern(p);
                if r.path.is_empty() {
                    self.warnings
                        .push("Repeat expression must have length > 0".to_string());
                }
                TypecheckResult::new(self.pow_path_type(&r.path, 0), r.env.to_group())
            }
        }
    }

    fn check_edge(&mut self, dir: EdgeDir, desc: &Option<Descriptor>) -> TypecheckResult {
        let t = self.refine_pattern_edge(dir, desc);
        let p = PathType::from_variable(&t, dir);
        let env = create_context(desc, t);
        TypecheckResult::new(p, env)
    }

    // -----------------------------------------------
    // Expression checking
    // -----------------------------------------------

    fn check_expr(&mut self, e: &Expr, env: &TypeEnvironment) -> SimpleType {
        match e {
            Expr::Const(v) => simple_type_of_value(v),

            Expr::Type(t) => t.clone(),

            Expr::AttrLookup { var, attr } => match env.get(var) {
                Some(t) => {
                    if matches!(t, VariableType::Zero) {
                        self.warnings
                            .push(format!("Variable {} is bound to empty type", var));
                        return SimpleType::Zero;
                    }
                    let at = t.get_attribute(attr);
                    if at.is_empty() {
                        self.warnings
                            .push(format!("Attribute {} not found in {}", attr, t));
                    }
                    at
                }
                None => {
                    self.errors
                        .push(format!("Variable {} not found in context", var));
                    SimpleType::Zero
                }
            },

            Expr::FieldAccess { base, field } => {
                let base_t = self.check_expr(base, env);
                match &base_t {
                    SimpleType::Record(fields) => match fields.get(field) {
                        Some(t) => t.clone(),
                        None => {
                            self.warnings
                                .push(format!("Field {} not found in record {}", field, base_t));
                            SimpleType::Zero
                        }
                    },
                    SimpleType::Star | SimpleType::Zero => base_t,
                    _ => {
                        self.warnings.push(format!(
                            "Field access {}.{} on non-record type {}",
                            "<expr>", field, base_t
                        ));
                        SimpleType::Zero
                    }
                }
            }

            Expr::Binop { op, left, right } => {
                let t1 = self.check_expr(left, env);

                // Type operations: rhs must be a Type literal in the well-formed case.
                match op {
                    BinOp::Is => {
                        if let Expr::Type(_) = right.as_ref() {
                            return SimpleType::B;
                        }
                    }
                    BinOp::As => {
                        if let Expr::Type(t) = right.as_ref() {
                            return t.clone();
                        }
                    }
                    _ => {}
                }

                let t2 = self.check_expr(right, env);
                let (expected_t1, expected_t2, result_t) = op.delta(&t1, &t2);

                if SimpleType::meet(&t1, &expected_t1) != SimpleType::Zero
                    && SimpleType::meet(&t2, &expected_t2) != SimpleType::Zero
                {
                    result_t
                } else {
                    self.warnings.push(format!(
                        "Binop {:?} between types {} and {} is not defined",
                        op, t1, t2
                    ));
                    SimpleType::Zero
                }
            }

            Expr::Unop { op, operand } => {
                let t = self.check_expr(operand, env);
                let (expected_t, result_t) = op.delta();

                if SimpleType::meet(&t, &expected_t) != SimpleType::Zero {
                    result_t
                } else {
                    self.warnings
                        .push(format!("Unop {:?} on type {} is not defined", op, t));
                    SimpleType::Zero
                }
            }

            Expr::IsNull { operand, .. } => {
                // The operand can be of any type — the test merely checks
                // for absence. Type-check it for diagnostics, then return
                // Bool.
                let _ = self.check_expr(operand, env);
                SimpleType::B
            }
        }
    }

    // -----------------------------------------------
    // Refinement helpers
    // -----------------------------------------------

    fn refine_pattern_node(&mut self, desc: &Option<Descriptor>) -> VariableType {
        let dtype = descriptor_type_of(desc);
        if let Some(d) = desc {
            self.assert_filters_drained(d);
        }
        let vt = VariableType::Node(dtype.clone());
        let refined = VariableType::refine(&self.schema, &vt);
        if matches!(refined, VariableType::Zero) {
            self.warnings
                .push(diagnose_node_mismatch(&self.schema, &dtype));
        }
        refined
    }

    fn refine_pattern_edge(&mut self, dir: EdgeDir, desc: &Option<Descriptor>) -> VariableType {
        let dtype = descriptor_type_of(desc);
        if let Some(d) = desc {
            self.assert_filters_drained(d);
        }
        // For any-direction edges (`-[L]-`), the paper defines refinement as
        // the join of the forward and undirected refinements. Refining only
        // as directional silently drops every undirected schema entry — e.g.
        // `knows` in LDBC, which is registered as non-directional.
        let refined = match dir {
            EdgeDir::Right | EdgeDir::Left => {
                VariableType::refine(&self.schema, &VariableType::edge_directional(dtype.clone()))
            }
            EdgeDir::None => VariableType::refine(
                &self.schema,
                &VariableType::edge_non_directional(dtype.clone()),
            ),
            EdgeDir::Any => {
                let t_fwd = VariableType::refine(
                    &self.schema,
                    &VariableType::edge_directional(dtype.clone()),
                );
                let t_und = VariableType::refine(
                    &self.schema,
                    &VariableType::edge_non_directional(dtype.clone()),
                );
                VariableType::join(&t_fwd, &t_und)
            }
        };
        if matches!(refined, VariableType::Zero) {
            self.warnings
                .push(diagnose_edge_mismatch(&self.schema, &dtype, dir));
        }
        refined
    }

    /// Elaboration must drain `Descriptor::value_filters` into `Filter`
    /// nodes before the typechecker runs. If we see leftovers it's a bug
    /// in elaboration; surface as an error so we don't silently ignore
    /// constraints.
    fn assert_filters_drained(&mut self, d: &Descriptor) {
        if !d.value_filters.is_empty() {
            self.errors.push(format!(
                "Descriptor for {:?} still carries value_filters at typecheck time \
                 (elaboration bug)",
                d.var
            ));
        }
    }

    /// After meeting two pattern contexts, surface any variable whose
    /// type collapsed to bottom. Each side's contribution is shown so
    /// the user can see the conflict directly. Pre-existing empties
    /// (already empty in `left` or `right`) are skipped to avoid
    /// double-warning.
    fn warn_for_collapsed_bindings(
        &mut self,
        merged: &TypeEnvironment,
        left: &TypeEnvironment,
        right: &TypeEnvironment,
    ) {
        for var in merged.keys() {
            let merged_t = match merged.get(var) {
                Some(t) => t,
                None => continue,
            };
            if !merged_t.is_empty() {
                continue;
            }
            let l_t = left.get(var);
            let r_t = right.get(var);
            let l_was_empty = l_t.is_some_and(VariableType::is_empty);
            let r_was_empty = r_t.is_some_and(VariableType::is_empty);
            if l_was_empty || r_was_empty {
                continue;
            }
            match (l_t, r_t) {
                (Some(l), Some(r)) => self.warnings.push(format!(
                    "variable {} cannot be both {} and {} under the active schema",
                    var,
                    short_var_type(l),
                    short_var_type(r)
                )),
                (Some(l), None) => self.warnings.push(format!(
                    "variable {} bound to {} collapses to empty under the active schema",
                    var,
                    short_var_type(l)
                )),
                (None, Some(r)) => self.warnings.push(format!(
                    "variable {} bound to {} collapses to empty under the active schema",
                    var,
                    short_var_type(r)
                )),
                (None, None) => {}
            }
        }
    }

    /// p^0 = identity (default node path), p^1 = p, p^n = meet(p, p^(n-1)).
    fn pow_path_type(&self, p: &PathType, n: u64) -> PathType {
        match n {
            0 => PathType::default(),
            1 => p.clone(),
            _ => PathType::meet(&self.schema, p, &self.pow_path_type(p, n - 1)),
        }
    }
}

/// Pull the `DescriptorType` out of an optional `Descriptor`. Anonymous
/// patterns like `()` or `-[]->` use the star descriptor.
fn descriptor_type_of(desc: &Option<Descriptor>) -> DescriptorType {
    match desc {
        Some(d) => d.dtype.clone(),
        None => DescriptorType::star(),
    }
}

/// Build the binding environment for a pattern position. Anonymous patterns
/// contribute no bindings.
fn create_context(desc: &Option<Descriptor>, t: VariableType) -> TypeEnvironment {
    match desc {
        Some(d) => TypeEnvironment::create_context(d, t),
        None => TypeEnvironment::new(),
    }
}

/// Map a literal `Value` to its `SimpleType`.
///
/// List and Record values get a deliberately loose type — the precise
/// element/field types would require recursive typing of values, which
/// fppc doesn't do (it has no list literals). Documented in
/// `docs/typechecker_migration.md` as a phase-1 punt.
fn simple_type_of_value(v: &Value) -> SimpleType {
    match v {
        // Null literal is the SQL untyped null: it inhabits every type for
        // the purpose of static checks. Mapping to `Star` keeps comparisons
        // like `x.attr = null` from collapsing the surrounding type
        // derivation to bottom; the runtime still drops the row via 3VL.
        Value::Null => SimpleType::Star,
        Value::Int(_) => SimpleType::Z,
        Value::Float(_) => SimpleType::F,
        Value::Str(_) => SimpleType::S,
        Value::Bool(_) => SimpleType::B,
        Value::List(_) => SimpleType::List(Box::new(SimpleType::Star)),
        Value::Record(_) => SimpleType::Star,
    }
}

// -----------------------------------------------
// Refinement diagnostics
// -----------------------------------------------
//
// When `refine` returns `VariableType::Zero` we know the pattern produces
// the empty type under the active schema, but the typechecker doesn't
// know which dimension caused the failure. These helpers re-run the
// match in two stages (label first, then properties) so the warning
// message can point at the actual source. Under `Schema::star()` the
// refine never returns Zero, so these never fire on permissive sessions.

/// Diagnose why a node descriptor refined to `Zero`. Splits the cause
/// into "no schema entry has a compatible label" vs "labels match but
/// the property record disagrees".
fn diagnose_node_mismatch(schema: &Schema, query: &DescriptorType) -> String {
    let label_compatible: Vec<&DescriptorType> = schema
        .nodes
        .iter()
        .filter_map(node_descriptor_ref)
        .filter(|d| LabelType::is_subtype(&d.label, &query.label))
        .collect();

    if label_compatible.is_empty() {
        format!(
            "node pattern (:{}) does not match any node type in the active schema (label not in schema)",
            query.label
        )
    } else {
        let expected = describe_property_alternatives(&label_compatible);
        format!(
            "node pattern (:{} {}) does not match the schema — properties differ (schema expects {})",
            query.label, query.props, expected
        )
    }
}

/// Diagnose why an edge descriptor refined to `Zero`. Stages: same
/// directionality → label match → property match. Endpoint mismatches
/// fall through to a generic message; surfacing those clearly would
/// require carrying the query's left/right node descriptors here, which
/// is left as future work.
fn diagnose_edge_mismatch(schema: &Schema, query: &DescriptorType, dir: EdgeDir) -> String {
    let (arrow_open, arrow_close) = match dir {
        EdgeDir::Right => ("-[:", "]->"),
        EdgeDir::Left => ("<-[:", "]-"),
        EdgeDir::None => ("~[:", "]~"),
        EdgeDir::Any => ("-[:", "]-"),
    };

    let same_dir: Vec<&DescriptorType> = schema
        .edges
        .iter()
        .filter_map(|vt| match (vt, dir) {
            (VariableType::EdgeDirectional { desc, .. }, EdgeDir::Right | EdgeDir::Left) => {
                Some(desc)
            }
            (VariableType::EdgeNonDirectional { desc, .. }, EdgeDir::None) => Some(desc),
            // Any-direction matches both kinds — the diagnostic reports against
            // the union, so any schema edge is a candidate.
            (VariableType::EdgeDirectional { desc, .. }, EdgeDir::Any)
            | (VariableType::EdgeNonDirectional { desc, .. }, EdgeDir::Any) => Some(desc),
            _ => None,
        })
        .collect();

    if same_dir.is_empty() {
        return format!(
            "edge pattern {arrow_open}{}{arrow_close} does not match any edge type (no schema edges with this directionality)",
            query.label
        );
    }

    let label_compatible: Vec<&DescriptorType> = same_dir
        .into_iter()
        .filter(|d| LabelType::is_subtype(&d.label, &query.label))
        .collect();

    if label_compatible.is_empty() {
        return format!(
            "edge pattern {arrow_open}{}{arrow_close} does not match any edge type (label not in schema)",
            query.label
        );
    }

    let prop_compatible: Vec<&&DescriptorType> = label_compatible
        .iter()
        .filter(|d| PropertyType::is_subtype(&d.props, &query.props))
        .collect();

    if prop_compatible.is_empty() {
        let expected = describe_property_alternatives(&label_compatible);
        return format!(
            "edge pattern {arrow_open}{} {}{arrow_close} does not match the schema — properties differ (schema expects {})",
            query.label, query.props, expected
        );
    }

    // Label and props OK individually but full edge subtype still failed:
    // the endpoint constraints don't match. Don't try to be too specific
    // here — we don't have the query's endpoint descriptors at this
    // shallow point.
    format!(
        "edge pattern {arrow_open}{}{arrow_close} does not match the schema — endpoint types do not match",
        query.label
    )
}

fn node_descriptor_ref(vt: &VariableType) -> Option<&DescriptorType> {
    match vt {
        VariableType::Node(d) => Some(d),
        _ => None,
    }
}

fn describe_property_alternatives(candidates: &[&DescriptorType]) -> String {
    let parts: Vec<String> = candidates.iter().map(|d| format!("{}", d.props)).collect();
    parts.join(" or ")
}

/// Compact rendering of a `VariableType` for warning text. The default
/// `Display` uses paper-style brackets (⸨...⸩); user-facing diagnostics
/// look cleaner with the bracket-style query syntax instead. Unions (a
/// common output from `refine` against schemas with overlapping label
/// combinations) are rendered as "(:A) or (:B)".
fn short_var_type(t: &VariableType) -> String {
    match t {
        VariableType::Node(d) => format!("(:{})", d.label),
        VariableType::EdgeDirectional { desc, .. } => format!("-[:{}]->", desc.label),
        VariableType::EdgeNonDirectional { desc, .. } => format!("~[:{}]~", desc.label),
        VariableType::Union(a, b) => format!("{} or {}", short_var_type(a), short_var_type(b)),
        VariableType::Group(inner) => format!("group<{}>", short_var_type(inner)),
        VariableType::Null => "Null".to_string(),
        VariableType::Zero => "⊥".to_string(),
    }
}
