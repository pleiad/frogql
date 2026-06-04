//! Typechecker for gqlite path patterns and queries.
//!
//! Ported from `fppc/src/typechecker/checker.rs`. Public surface mirrors
//! fppc's `Typechecker` / `TypecheckResult`; the differences are documented
//! in `docs/internals/typechecker_migration.md`.

use std::collections::HashMap;

use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::path_prefix::{PathPrefix, PathSearch, UnboundedSupport};
use crate::syntax::query::{Aggregator, MatchStatement, Query, ReturnItem, SortKey, SortSpec};

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
    /// Variables bound locally by an enclosing list comprehension
    /// (`[x IN ... | ...]`). The source list types as `List(Star)`, so the
    /// element type is not tracked precisely; a name in this stack resolves
    /// to `Star` in `Var` / `AttrLookup` instead of erroring as unbound.
    comprehension_scope: Vec<String>,
    /// Outer environment(s) visible inside a correlated subquery body
    /// (`EXISTS`/`VALUE`). Merged into a `Filter` predicate's environment so
    /// the body's WHERE can reference outer-bound variables. A stack to
    /// support nesting. Empty outside subquery bodies, so a top-level
    /// per-clause WHERE keeps its strict pattern-local scope.
    ambient_env: Vec<TypeEnvironment>,
}

impl Typechecker {
    pub fn new(schema: Schema) -> Self {
        Typechecker {
            schema,
            errors: Vec::new(),
            warnings: Vec::new(),
            comprehension_scope: Vec::new(),
            ambient_env: Vec::new(),
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

        self.check_unbounded_repetition(q);
        self.check_selective_isolation(q);

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
        if let Some(specs) = &q.order_by {
            self.check_order_by(specs, q.returns.as_deref(), &r.env);
        }

        if !self.errors.is_empty() {
            r.ok = false;
        }
        r
    }

    /// ISO §16.17 + §22.14: enforce comparable-value-type per CR 1
    /// (no Feature GA04). Mixed Expr+Column sort keys cannot be
    /// served by either pre- or post-projection sort.
    fn check_order_by(
        &mut self,
        specs: &[SortSpec],
        returns: Option<&[ReturnItem]>,
        env: &TypeEnvironment,
    ) {
        let has_expr = specs.iter().any(|s| matches!(s.key, SortKey::Expr(_)));
        let has_column = specs.iter().any(|s| {
            matches!(
                s.key,
                SortKey::Column(_) | SortKey::ColumnCast { .. } | SortKey::ColumnField { .. }
            )
        });
        if has_expr && has_column {
            self.errors.push(
                "ORDER BY mixes RETURN-alias references and free expressions; either \
                 use only aliases (each sort target with `AS <name>` in RETURN) or \
                 only free expressions over the binding table."
                    .into(),
            );
        }

        for spec in specs {
            let t = match &spec.key {
                SortKey::Expr(e) => self.check_expr(e, env),
                SortKey::Column(idx) => match returns.and_then(|rs| rs.get(*idx)) {
                    Some(ReturnItem::Expr { expr, .. }) => self.check_expr(expr, env),
                    // Aggregate output typing is a separate gap; pass-through.
                    Some(ReturnItem::Aggregate { .. }) => SimpleType::Star,
                    None => {
                        self.errors.push(format!(
                            "ORDER BY column reference #{idx} is out of bounds for the \
                             RETURN clause (only {} item(s) projected)",
                            returns.map(|rs| rs.len()).unwrap_or(0),
                        ));
                        continue;
                    }
                },
                SortKey::ColumnCast { col, ty } => {
                    if returns.and_then(|rs| rs.get(*col)).is_none() {
                        self.errors.push(format!(
                            "ORDER BY column reference #{col} is out of bounds for the \
                             RETURN clause (only {} item(s) projected)",
                            returns.map(|rs| rs.len()).unwrap_or(0),
                        ));
                        continue;
                    }
                    ty.clone()
                }
                SortKey::ColumnField { col, path } => {
                    // Type the projected column, then walk the record path.
                    let mut t = match returns.and_then(|rs| rs.get(*col)) {
                        Some(ReturnItem::Expr { expr, .. }) => self.check_expr(expr, env),
                        Some(ReturnItem::Aggregate { .. }) => SimpleType::Star,
                        None => {
                            self.errors.push(format!(
                                "ORDER BY column reference #{col} is out of bounds for the \
                                 RETURN clause (only {} item(s) projected)",
                                returns.map(|rs| rs.len()).unwrap_or(0),
                            ));
                            continue;
                        }
                    };
                    for field in path {
                        t = match &t {
                            SimpleType::Record(fields) => {
                                fields.get(field).cloned().unwrap_or(SimpleType::Zero)
                            }
                            SimpleType::Star | SimpleType::Zero => t.clone(),
                            _ => {
                                self.errors.push(format!(
                                    "ORDER BY field `.{field}` accesses a non-record type {t}"
                                ));
                                SimpleType::Zero
                            }
                        };
                    }
                    t
                }
            };
            if !is_orderable_per_iso_22_14(&t) {
                self.errors.push(format!(
                    "ORDER BY sort key `{}` has type {} which is not a comparable \
                     value type per ISO §22.14 Conformance Rule 1; expected a scalar \
                     (Int, Float, Bool, Str) — gqlite does not implement Feature GA04 \
                     'Universal comparison'.",
                    spec.key, t,
                ));
            }
        }
    }

    /// Reject unbounded repetition unless its nearest prefix makes it finite.
    fn check_unbounded_repetition(&mut self, q: &Query) {
        for m in &q.matches {
            self.check_unbounded_in(m.pattern(), None);
        }
    }

    fn check_unbounded_in(&mut self, p: &PathPattern, prefix: Option<PathPrefix>) {
        match p {
            PathPattern::Selected {
                prefix: pp,
                pattern,
            } => self.check_unbounded_in(pattern, Some(*pp)),
            PathPattern::Repeat { pattern, lb, ub } => {
                if ub.is_none() {
                    self.validate_unbounded(*lb, prefix);
                }
                self.check_unbounded_in(pattern, prefix);
            }
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                self.check_unbounded_in(a, prefix);
                self.check_unbounded_in(b, prefix);
            }
            PathPattern::Filter(inner, _) | PathPattern::Questioned(inner) => {
                self.check_unbounded_in(inner, prefix)
            }
            // The path-variable wrapper is transparent: keep walking with
            // the same prefix (it sits outside any `Selected` prefix).
            PathPattern::Named { pattern, .. } => self.check_unbounded_in(pattern, prefix),
            PathPattern::Node(_)
            | PathPattern::EdgeRight(_)
            | PathPattern::EdgeLeft(_)
            | PathPattern::EdgeUndirected(_)
            | PathPattern::EdgeAnyDirection(_) => {}
        }
    }

    fn validate_unbounded(&mut self, lb: usize, prefix: Option<PathPrefix>) {
        match prefix.and_then(|p| p.unbounded_support()) {
            None => self.errors.push(
                "Unbounded repetition (`*`, `+`, `{n,}`) is only supported under a \
                 SHORTEST-family path search prefix (`ANY SHORTEST`, `ALL SHORTEST`, \
                 `SHORTEST n [PATHS]`, `SHORTEST n GROUPS`) or a restrictive path \
                 mode (`ACYCLIC`, `SIMPLE`, `TRAIL`). Bounded repetition `{n,m}` \
                 works without any prefix."
                    .to_string(),
            ),
            Some(UnboundedSupport::Shortest { .. }) => {
                if lb > 1 {
                    self.errors.push(
                        "SHORTEST over unbounded repetition supports `*` and `+` only; \
                         `{n,}` with n >= 2 needs a restrictive path mode (`ACYCLIC` / \
                         `SIMPLE` / `TRAIL`) instead."
                            .to_string(),
                    );
                }
            }
            Some(UnboundedSupport::Mode(_)) => {}
        }
    }

    /// ISO §16.6 SR 5-8: selective patterns may share only boundary vars.
    fn check_selective_isolation(&mut self, q: &Query) {
        let mut global: HashMap<String, usize> = HashMap::new();
        for m in &q.matches {
            count_var_occurrences(m.pattern(), &mut global);
        }

        let mut selected: Vec<&PathPattern> = Vec::new();
        for m in &q.matches {
            collect_selected(m.pattern(), &mut selected);
        }

        for s in selected {
            let PathPattern::Selected { prefix, pattern } = s else {
                continue;
            };
            if prefix.search == PathSearch::All {
                continue;
            }

            let (left, right) = boundary_node_vars(pattern);
            let mut local: HashMap<String, usize> = HashMap::new();
            count_var_occurrences(pattern, &mut local);

            let mut shared: Vec<String> = local
                .iter()
                .filter(|(v, _)| Some(*v) != left.as_ref() && Some(*v) != right.as_ref())
                .filter(|(v, &lc)| global.get(*v).copied().unwrap_or(0) > lc)
                .map(|(v, _)| v.clone())
                .collect();

            if !shared.is_empty() {
                shared.sort();
                self.errors.push(format!(
                    "Selective path pattern (a SHORTEST/ANY pattern) shares interior \
                     variable(s) `{}` with the rest of the query. ISO §16.6 SR 7 requires a \
                     selective pattern to be evaluable in isolation: only its boundary \
                     (endpoint) variables may join other patterns. Rename the interior \
                     variable(s), or restructure so the shared variable is an endpoint.",
                    shared.join("`, `")
                ));
            }
        }
    }

    /// Sequential walk of the match chain when at least one is OPTIONAL.
    /// Implements TSeq composed with TMatch / TOpt: each Simple meets, each
    /// Optional left-joins.
    ///
    /// Path types are not threaded across MATCH statements. They track the
    /// shape of a single concatenation (used for short-circuiting on Zero);
    /// joins and left-joins produce independent paths whose shapes are
    /// unrelated, so we keep the first match's path for short-circuiting on
    /// its own emptiness and rely on the environment for everything else.
    fn check_match_chain(&mut self, matches: &[MatchStatement]) -> TypecheckResult {
        let mut iter = matches.iter();
        let first = iter
            .next()
            .expect("Query::matches must contain at least one match statement");
        let first_r = self.check_path_pattern(first.pattern());
        // For a leading OPTIONAL, every variable it introduces gains Null
        // per TLEFTJOIN with Γ₁ = ∅.
        let (mut env, path) = if first.is_optional() {
            let acc =
                TypeEnvironment::outer_join(&self.schema, &TypeEnvironment::new(), &first_r.env);
            (acc, first_r.path)
        } else {
            (first_r.env, first_r.path)
        };

        for m in iter {
            let r = self.check_path_pattern(m.pattern());
            env = match m {
                MatchStatement::Simple { .. } => {
                    match TypeEnvironment::meet(&self.schema, &env, &r.env) {
                        Ok(e) => {
                            self.warn_for_collapsed_bindings(&e, &env, &r.env);
                            e
                        }
                        Err(e) => {
                            self.errors
                                .push(format!("Concatenation of contexts failed: {}", e));
                            env
                        }
                    }
                }
                MatchStatement::Optional { .. } => {
                    TypeEnvironment::outer_join(&self.schema, &env, &r.env)
                }
            };
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

    /// ISO §14.x grouped query: every non-aggregate RETURN item must be
    /// functionally determined by the grouping keys. Two valid shapes — (a)
    /// the item structurally equals a grouping key (covers a property /
    /// expression key like `GROUP BY x.city` + `RETURN x.city`), or (b)
    /// every binding variable the item references is itself a grouping key
    /// spelled as a *bare* `<binding variable reference>` (covers grouping
    /// by node identity, `GROUP BY liker` + `RETURN liker.firstName`).
    /// Grouping by a *property* (`x.city`) does NOT make all of `x`'s other
    /// attributes available, so only bare `Expr::Var` keys seed shape (b).
    /// Aggregate-bearing and subquery-bearing items are exempt (reduced or
    /// evaluated over the group on its representative row).
    fn check_returns_match_group_by(&mut self, items: &[ReturnItem], group_by: &[Expr]) {
        let grouped_node_vars: std::collections::BTreeSet<String> = group_by
            .iter()
            .filter_map(|g| match g {
                Expr::Var(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        for item in items {
            if let ReturnItem::Expr { expr, .. } = item {
                // Reduced over the group (`COUNT(x)+COUNT(y)`) or evaluated
                // on the representative row (`VALUE { ... }`, `EXISTS { ... }`)
                // — neither needs to be a grouping key.
                if expr.contains_agg() || expr.contains_subquery() {
                    continue;
                }
                if group_by.iter().any(|g| g == expr) {
                    continue;
                }
                let mut refs = std::collections::BTreeSet::new();
                expr.referenced_vars(&mut refs);
                if !refs.is_empty() && refs.iter().all(|v| grouped_node_vars.contains(v)) {
                    continue;
                }
                self.errors.push(format!(
                    "RETURN item `{expr}` is not functionally determined by the GROUP BY \
                     keys; a non-aggregate projection must match a grouping key or depend \
                     only on grouped binding variables."
                ));
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
                ReturnItem::Aggregate { agg, .. } => {
                    let _ = self.check_aggregator(agg, env);
                }
            }
        }
    }

    /// Type an aggregate function (ISO §20.9 GR 7). The inner expr is
    /// checked for diagnostics; the returned type is the reducer's
    /// result type so it composes inside arithmetic.
    fn check_aggregator(&mut self, agg: &Aggregator, env: &TypeEnvironment) -> SimpleType {
        use crate::syntax::query::GeneralSetKind;
        let num = SimpleType::union(&SimpleType::Z, &SimpleType::F);
        match agg {
            // COUNT(*) is a cardinality — always Int, no inner expr.
            Aggregator::CountStar => SimpleType::Z,
            Aggregator::GeneralSet { kind, expr, .. } => {
                let inner = self.check_expr(expr, env);
                match kind {
                    GeneralSetKind::Count => SimpleType::Z,
                    GeneralSetKind::Avg => SimpleType::F,
                    GeneralSetKind::Sum => num,
                    // MIN/MAX preserve the element type.
                    GeneralSetKind::Min | GeneralSetKind::Max => inner,
                    // COLLECT_LIST gathers the element type into a list.
                    GeneralSetKind::CollectList => SimpleType::List(Box::new(inner)),
                }
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

            PathPattern::Concat(p1, p2) => {
                // TConcat: paths share an endpoint, so `PathType::meet`
                // composes their shapes.
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

            PathPattern::Join(p1, p2) => {
                // TJoin: emptiness is `e1 ∨ e2`, env is `Γ1 ⊓ Γ2`. No
                // path-meet — `(x:A), (y:B)` describes independent
                // variables; meeting their labels would falsely
                // collapse to Zero. Same class of bug Toro fixed for
                // multi-MATCH in `354eaf6`.
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

                let p = if r1.path.is_unsatisfiable() || r2.path.is_unsatisfiable() {
                    PathType::Zero
                } else {
                    r1.path
                };
                TypecheckResult::new(p, cm)
            }

            PathPattern::Filter(pattern, expr) => {
                let r = self.check_path_pattern(pattern);
                // Inside a correlated subquery body, the predicate may
                // reference outer-bound variables; merge the ambient outer
                // environment under the pattern's own bindings.
                let filter_env = match self.ambient_env.last() {
                    Some(outer) => {
                        let mut merged = outer.clone();
                        for k in r.env.keys() {
                            if let Some(v) = r.env.get(k) {
                                merged.set(k, v.clone());
                            }
                        }
                        merged
                    }
                    None => r.env.clone(),
                };
                let t = self.check_expr(expr, &filter_env);

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

            // Prefixes filter paths; they do not transform variable types.
            PathPattern::Selected { pattern, .. } => self.check_path_pattern(pattern),

            // A path-variable declaration is transparent to the pattern's
            // own typing: check the inner operand, then add a single inert
            // `Path` binding for `var` to the environment. The PathType is
            // unchanged (the path's shape is the operand's shape).
            PathPattern::Named { var, pattern } => {
                let r = self.check_path_pattern(pattern);
                let mut env = r.env;
                env.set(var, VariableType::Path);
                TypecheckResult::new(r.path, env)
            }

            PathPattern::Repeat { pattern, lb, ub } => {
                let r = self.check_path_pattern(pattern);
                let raw_lb = *lb as u64;

                let effective_lb = if !r.path.is_empty() {
                    raw_lb.min(3)
                } else if ub.is_none() {
                    // An empty-matching inner under *unbounded* repetition is
                    // non-terminating at runtime (every extra lap adds zero
                    // length, so a SHORTEST-GROUPS / TRAIL search never fills
                    // its budget). This must be a hard error, not a warning:
                    // the runtime's finite-evaluation path relies on each
                    // application contributing at least one edge.
                    self.errors.push(
                        "Unbounded repetition (`*`, `+`, `{n,}`) requires an inner pattern \
                         of length > 0; an inner that can match the empty path makes the \
                         repetition non-terminating."
                            .to_string(),
                    );
                    raw_lb
                } else {
                    // Bounded repetition with an empty-matching inner is
                    // degenerate but terminates, so it stays a warning.
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

            // ISO §20.12: a `<binding variable reference>` resolves to
            // the value type of the bound variable. `Group` (paths from
            // repetition `{n,m}`) is not yet projectable as a value;
            // documented as a future gap.
            Expr::Var(name) => {
                if self.comprehension_scope.iter().any(|v| v == name) {
                    return SimpleType::Star;
                }
                match env.get(name) {
                    Some(t) => variable_type_to_simple_type(t),
                    None => {
                        self.errors
                            .push(format!("Variable {} not found in context", name));
                        SimpleType::Zero
                    }
                }
            }

            Expr::AttrLookup { var, attr } if self.comprehension_scope.iter().any(|v| v == var) => {
                let _ = attr;
                // A comprehension element's attributes are not statically
                // typed (element type is `Star`); resolve to `Star`.
                SimpleType::Star
            }
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

            Expr::Coalesce(args) => {
                // ISO §20.7 SR 1c-1d: result type is the union of arg types.
                let mut iter = args.iter();
                let first = iter.next().expect("parser ensures ≥ 2 args");
                let mut acc = self.check_expr(first, env);
                for a in iter {
                    let t = self.check_expr(a, env);
                    acc = SimpleType::union(&acc, &t);
                }
                acc
            }

            Expr::Call { name, args } => {
                // Built-in scalar functions. Inner args are checked for
                // diagnostics (and reused below); the result type follows
                // ISO function rules.
                let arg_types: Vec<SimpleType> =
                    args.iter().map(|a| self.check_expr(a, env)).collect();
                match name.as_str() {
                    // FLOOR(numeric) → Float (the runtime narrows via CAST).
                    "FLOOR" => SimpleType::F,
                    // CAST(operand AS <value type>) → the target type, which
                    // the parser carries as `Expr::Type` in args[1].
                    "CAST" => match args.get(1) {
                        Some(Expr::Type(t)) => t.clone(),
                        _ => {
                            self.errors
                                .push("CAST is missing its target type".to_string());
                            SimpleType::Zero
                        }
                    },
                    // ISO §20.16 path functions (plus the non-standard
                    // NODES/EDGES translation helpers). The single argument
                    // must be a PATH; a provably non-path argument (e.g. a
                    // node variable) is a hard type error. `Star` is
                    // tolerated, matching the gradual rule elsewhere.
                    "ELEMENTS" | "NODES" | "EDGES" | "PATH_LENGTH" | "CARDINALITY" => {
                        match arg_types.first() {
                            Some(arg_t)
                                if SimpleType::meet(arg_t, &SimpleType::Path).is_empty() =>
                            {
                                self.errors
                                    .push(format!("{name} expects a PATH argument, got {arg_t}"));
                            }
                            None => self
                                .errors
                                .push(format!("{name} expects one PATH argument, got none")),
                            _ => {}
                        }
                        match name.as_str() {
                            // PATH_LENGTH (edge count) / CARDINALITY (element
                            // count) are integers.
                            "PATH_LENGTH" | "CARDINALITY" => SimpleType::Z,
                            // ELEMENTS / NODES / EDGES are lists of node/edge
                            // reference values.
                            _ => SimpleType::List(Box::new(SimpleType::Star)),
                        }
                    }
                    other => {
                        self.errors
                            .push(format!("unknown built-in function `{other}`"));
                        SimpleType::Zero
                    }
                }
            }

            Expr::Case {
                branches,
                else_expr,
            } => {
                let mut acc = SimpleType::Zero;
                for (cond, value) in branches {
                    let cond_t = self.check_expr(cond, env);
                    if SimpleType::meet(&cond_t, &SimpleType::B) == SimpleType::Zero {
                        self.warnings
                            .push(format!("CASE WHEN condition has non-bool type {cond_t}"));
                    }
                    let value_t = self.check_expr(value, env);
                    acc = SimpleType::union(&acc, &value_t);
                }
                if let Some(value) = else_expr {
                    let else_t = self.check_expr(value, env);
                    acc = SimpleType::union(&acc, &else_t);
                } else {
                    acc = SimpleType::union(&acc, &SimpleType::Zero);
                }
                acc
            }

            Expr::ListComprehension {
                var,
                source,
                filter,
                body,
            } => {
                // `source` is evaluated in the current scope and must be a
                // list (lenient: `Star` and `List(_)` both pass).
                let src_t = self.check_expr(source, env);
                if !matches!(src_t, SimpleType::List(_) | SimpleType::Star)
                    && SimpleType::meet(&src_t, &SimpleType::Star) != src_t
                {
                    self.warnings.push(format!(
                        "list comprehension source has non-list type {src_t}"
                    ));
                }
                // `var` is in scope for `filter`/`body` only.
                self.comprehension_scope.push(var.clone());
                if let Some(f) = filter {
                    let f_t = self.check_expr(f, env);
                    if SimpleType::meet(&f_t, &SimpleType::B) == SimpleType::Zero {
                        self.warnings
                            .push(format!("list comprehension WHERE has non-bool type {f_t}"));
                    }
                }
                let body_t = self.check_expr(body, env);
                self.comprehension_scope.pop();
                SimpleType::List(Box::new(body_t))
            }

            Expr::Record { fields } => {
                // A record constructor types as a closed record over the
                // field expression types.
                let mut m = std::collections::BTreeMap::new();
                for (k, e) in fields {
                    m.insert(k.clone(), self.check_expr(e, env));
                }
                SimpleType::Record(m)
            }

            Expr::ValueSubquery { body } => {
                // Type the correlated body under the outer environment,
                // then type its single RETURN item against the resulting
                // inner environment — that is the value the subquery yields.
                let r = self.check_subquery_body(body, env);
                match body.returns.as_deref().and_then(|items| items.first()) {
                    Some(ReturnItem::Expr { expr, .. }) => self.check_expr(expr, &r.env),
                    Some(ReturnItem::Aggregate { agg, .. }) => self.check_aggregator(agg, &r.env),
                    None => {
                        self.errors
                            .push("VALUE subquery has no RETURN item".to_string());
                        SimpleType::Zero
                    }
                }
            }

            Expr::Exists { body } | Expr::NotExists { body } => {
                // TExists / TNotExists: type the body under the outer
                // environment so correlated variables resolve, but
                // discard the inner environment afterwards — bindings
                // introduced by the body do not escape. Both predicates
                // return Bool regardless of body emptiness; the
                // optimisation that folds an empty body to a literal
                // happens later.
                let _ = self.check_subquery_body(body, env);
                SimpleType::B
            }

            // ISO §20.9: an aggregate as a value-expression operand
            // (`COUNT(x) + COUNT(y)`). Type-check the inner expr for
            // diagnostics, then return the aggregate's result type so the
            // surrounding arithmetic typechecks.
            Expr::Agg(agg) => self.check_aggregator(agg, env),
        }
    }

    /// Type-check the body of an existential subquery. Starts from a
    /// clone of the outer environment so outer-bound variables are
    /// visible inside (correlation), then walks the match chain using
    /// the same TJoin / TLEFTJOIN operators as the top-level query
    /// checker. Returns the inner result for use by the optimisation
    /// step (Phase 2); callers that only need Bool can ignore it.
    fn check_subquery_body(&mut self, q: &Query, outer: &TypeEnvironment) -> TypecheckResult {
        let mut env = outer.clone();
        let mut path: Option<PathType> = None;
        // Make the outer environment available to the body's `Filter`
        // predicates so a correlated WHERE (e.g. `WHERE x IN NODES(path)`)
        // can reference outer-bound variables. The runtime evaluates such a
        // body per outer row with the same correlation.
        self.ambient_env.push(outer.clone());
        for m in &q.matches {
            let r = self.check_path_pattern(m.pattern());
            if path.is_none() {
                path = Some(r.path.clone());
            }
            env = match m {
                MatchStatement::Simple { .. } => {
                    match TypeEnvironment::meet(&self.schema, &env, &r.env) {
                        Ok(e) => e,
                        Err(msg) => {
                            self.errors.push(format!(
                                "EXISTS body: concatenation of contexts failed: {msg}"
                            ));
                            env
                        }
                    }
                }
                MatchStatement::Optional { .. } => {
                    TypeEnvironment::outer_join(&self.schema, &env, &r.env)
                }
            };
        }
        self.ambient_env.pop();
        let path = path.unwrap_or(PathType::Zero);
        let mut r = TypecheckResult::new(path, env);
        r.empty = r.path.is_unsatisfiable() || r.env.is_empty();
        r
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
                VariableType::join(t_fwd, t_und)
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
/// `docs/internals/typechecker_migration.md` as a phase-1 punt.
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
        Value::Node(_) => SimpleType::Node,
        Value::Edge(_) => SimpleType::Edge,
        Value::Path(_) => SimpleType::Path,
    }
}

/// Lift a `VariableType` to its `SimpleType` for `<binding variable
/// reference>` (ISO §20.12). Edges of any direction collapse to
/// `Edge`; reference values lose their descriptor here because the
/// SimpleType lattice has no way to carry it (a future refinement
/// could add `SimpleType::Node(DescriptorType)`).
fn variable_type_to_simple_type(t: &VariableType) -> SimpleType {
    match t {
        VariableType::Node(_) => SimpleType::Node,
        VariableType::EdgeDirectional { .. } | VariableType::EdgeNonDirectional { .. } => {
            SimpleType::Edge
        }
        VariableType::Union(a, b) => SimpleType::union(
            &variable_type_to_simple_type(a),
            &variable_type_to_simple_type(b),
        ),
        VariableType::Null => SimpleType::Star,
        VariableType::Path => SimpleType::Path,
        VariableType::Zero => SimpleType::Zero,
        // Repetition-grouping is not projectable as a single value;
        // typing it as Star defers the runtime check (`Expr::Var` on
        // a Group produces a Failure → Null, which is acceptable for
        // expressions but not yet for projection).
        VariableType::Group(_) => SimpleType::Star,
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
        VariableType::Path => "path".to_string(),
        VariableType::Zero => "⊥".to_string(),
    }
}

/// ISO §22.14 CR 1 + §4.4.2: `Star` deferred to runtime; `Zero` is
/// surfaced as a typo / schema mismatch; lists / records / groups
/// would need Feature GA04.
fn is_orderable_per_iso_22_14(t: &SimpleType) -> bool {
    match t {
        SimpleType::Z | SimpleType::F | SimpleType::B | SimpleType::S | SimpleType::Star => true,
        SimpleType::Zero => false,
        SimpleType::Union(a, b) => is_orderable_per_iso_22_14(a) && is_orderable_per_iso_22_14(b),
        // ISO §4.4.4 says reference values of the same base type are
        // *equality*-comparable, not ordering-comparable; without GA04
        // they cannot be sort keys.
        SimpleType::List(_)
        | SimpleType::Record(_)
        | SimpleType::Group(_)
        | SimpleType::Node
        | SimpleType::Edge
        | SimpleType::Path => false,
    }
}

/// Boundary variables are the first/last top-level node variables.
fn boundary_node_vars(p: &PathPattern) -> (Option<String>, Option<String>) {
    let mut vars: Vec<String> = Vec::new();
    collect_top_node_vars(p, &mut vars);
    let left = vars.first().cloned();
    let right = vars.last().cloned();
    (left, right)
}

fn collect_top_node_vars(p: &PathPattern, out: &mut Vec<String>) {
    match p {
        PathPattern::Node(d) => {
            if let Some(v) = d.as_ref().and_then(|d| d.var.clone()) {
                out.push(v);
            }
        }
        PathPattern::Concat(a, b) => {
            collect_top_node_vars(a, out);
            collect_top_node_vars(b, out);
        }
        PathPattern::Filter(inner, _) | PathPattern::Named { pattern: inner, .. } => {
            collect_top_node_vars(inner, out)
        }
        PathPattern::EdgeRight(_)
        | PathPattern::EdgeLeft(_)
        | PathPattern::EdgeUndirected(_)
        | PathPattern::EdgeAnyDirection(_)
        | PathPattern::Repeat { .. }
        | PathPattern::Questioned(_)
        | PathPattern::Union(_, _)
        | PathPattern::Join(_, _)
        | PathPattern::Selected { .. } => {}
    }
}

/// Count element variable declarations; WHERE references do not bind.
fn count_var_occurrences(p: &PathPattern, out: &mut HashMap<String, usize>) {
    let bump = |d: &Option<Descriptor>, out: &mut HashMap<String, usize>| {
        if let Some(v) = d.as_ref().and_then(|d| d.var.clone()) {
            *out.entry(v).or_insert(0) += 1;
        }
    };
    match p {
        PathPattern::Node(d)
        | PathPattern::EdgeRight(d)
        | PathPattern::EdgeLeft(d)
        | PathPattern::EdgeUndirected(d)
        | PathPattern::EdgeAnyDirection(d) => bump(d, out),
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            count_var_occurrences(a, out);
            count_var_occurrences(b, out);
        }
        PathPattern::Filter(inner, _)
        | PathPattern::Questioned(inner)
        | PathPattern::Repeat { pattern: inner, .. }
        | PathPattern::Selected { pattern: inner, .. }
        | PathPattern::Named { pattern: inner, .. } => count_var_occurrences(inner, out),
    }
}

fn collect_selected<'a>(p: &'a PathPattern, out: &mut Vec<&'a PathPattern>) {
    match p {
        PathPattern::Selected { pattern, .. } => {
            out.push(p);
            collect_selected(pattern, out);
        }
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            collect_selected(a, out);
            collect_selected(b, out);
        }
        PathPattern::Filter(inner, _)
        | PathPattern::Questioned(inner)
        | PathPattern::Repeat { pattern: inner, .. }
        | PathPattern::Named { pattern: inner, .. } => collect_selected(inner, out),
        PathPattern::Node(_)
        | PathPattern::EdgeRight(_)
        | PathPattern::EdgeLeft(_)
        | PathPattern::EdgeUndirected(_)
        | PathPattern::EdgeAnyDirection(_) => {}
    }
}

#[cfg(test)]
mod orderable_tests {
    use super::is_orderable_per_iso_22_14;
    use super::SimpleType;
    use std::collections::BTreeMap;

    #[test]
    fn scalars_and_star_are_orderable() {
        for t in [
            SimpleType::Z,
            SimpleType::F,
            SimpleType::B,
            SimpleType::S,
            SimpleType::Star,
        ] {
            assert!(
                is_orderable_per_iso_22_14(&t),
                "{t:?} should be orderable per §22.14"
            );
        }
    }

    #[test]
    fn zero_is_not_orderable() {
        // Zero typically comes from a strict-schema lookup of an
        // undeclared attribute, or from a contradictory constraint.
        // Either way the sort key cannot meaningfully order rows.
        assert!(!is_orderable_per_iso_22_14(&SimpleType::Zero));
    }

    #[test]
    fn lists_and_records_are_not_orderable_without_ga04() {
        assert!(!is_orderable_per_iso_22_14(&SimpleType::List(Box::new(
            SimpleType::Z
        ))));
        let mut fields = BTreeMap::new();
        fields.insert("k".into(), SimpleType::S);
        assert!(!is_orderable_per_iso_22_14(&SimpleType::Record(fields)));
        assert!(!is_orderable_per_iso_22_14(&SimpleType::Group(Box::new(
            SimpleType::Z
        ))));
    }

    #[test]
    fn union_orderable_iff_both_sides_orderable() {
        // int|str: both scalar → orderable in our model. ISO §4.4.2
        // would actually class this as not-essentially-comparable,
        // but our value_cmp returns None on cross-kind, which falls
        // through to the static accept here. A stricter pass could
        // reject cross-kind unions; documented as a known follow-up.
        let int_or_str = SimpleType::Union(Box::new(SimpleType::Z), Box::new(SimpleType::S));
        assert!(is_orderable_per_iso_22_14(&int_or_str));

        // int|list rejects.
        let int_or_list = SimpleType::Union(
            Box::new(SimpleType::Z),
            Box::new(SimpleType::List(Box::new(SimpleType::Z))),
        );
        assert!(!is_orderable_per_iso_22_14(&int_or_list));
    }
}
