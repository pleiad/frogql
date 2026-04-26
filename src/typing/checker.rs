//! Typechecker for gqlite path patterns and queries.
//!
//! Ported from `fppc/src/typechecker/checker.rs`. Public surface mirrors
//! fppc's `Typechecker` / `TypecheckResult`; the differences are documented
//! in `docs/typechecker_migration.md`.

use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::Query;

use super::descriptor_type::DescriptorType;
use super::path_type::{EdgeDir, PathType};
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

    fn failed() -> Self {
        TypecheckResult {
            path: PathType::Zero,
            env: TypeEnvironment::new(),
            ok: false,
            empty: true,
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

    /// Type-check a full `Query`. Elaboration has already folded any
    /// `WHERE` clause into `PathPattern::Filter` nodes inside `q.pattern`,
    /// so this just checks the pattern. `RETURN` is not type-checked in
    /// this phase (out-of-scope: result-row typing).
    pub fn check_query(&mut self, q: &Query) -> TypecheckResult {
        self.check_pattern(&q.pattern)
    }

    /// Type-check a `PathPattern`.
    pub fn check_pattern(&mut self, p: &PathPattern) -> TypecheckResult {
        self.errors.clear();
        self.warnings.clear();
        let mut r = self.check_path_pattern(p);
        if !self.errors.is_empty() {
            r.ok = false;
        }
        r.empty = r.path.is_unsatisfiable() || r.env.is_empty();
        r
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
                        self.errors.push(format!(
                            "Concatenation of contexts failed: {}",
                            e
                        ));
                        r1.env.clone()
                    }
                };

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

            PathPattern::Repeat { pattern, lb, ub: _ub } => {
                let r = self.check_path_pattern(pattern);
                let raw_lb = *lb as u64;

                let effective_lb = if !r.path.is_empty() {
                    raw_lb.min(3)
                } else {
                    self.warnings
                        .push("Repeat expression must have length > 0".to_string());
                    raw_lb
                };

                TypecheckResult::new(
                    self.pow_path_type(&r.path, effective_lb),
                    r.env.to_group(),
                )
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
        let vt = VariableType::Node(dtype);
        VariableType::refine(&self.schema, &vt)
    }

    fn refine_pattern_edge(
        &mut self,
        dir: EdgeDir,
        desc: &Option<Descriptor>,
    ) -> VariableType {
        let dtype = descriptor_type_of(desc);
        if let Some(d) = desc {
            self.assert_filters_drained(d);
        }
        let vt = match dir {
            EdgeDir::Right | EdgeDir::Left | EdgeDir::Any => {
                VariableType::edge_directional(dtype)
            }
            EdgeDir::None => VariableType::edge_non_directional(dtype),
        };
        VariableType::refine(&self.schema, &vt)
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
        Value::Int(_) => SimpleType::Z,
        Value::Float(_) => SimpleType::F,
        Value::Str(_) => SimpleType::S,
        Value::Bool(_) => SimpleType::B,
        Value::List(_) => SimpleType::List(Box::new(SimpleType::Star)),
        Value::Record(_) => SimpleType::Star,
    }
}

