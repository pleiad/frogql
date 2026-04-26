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

    /// Type-check a full `Query`. The `WHERE` clause is applied as a final
    /// filter against the pattern's environment; the `RETURN` clause is not
    /// type-checked in this phase (out-of-scope: result-row typing).
    pub fn check_query(&mut self, q: &Query) -> TypecheckResult {
        let _ = q;
        let _ = &self.schema;
        let _ = &mut self.errors;
        let _ = &mut self.warnings;
        todo!("check_query: Step 7")
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

    fn check_path_pattern(&mut self, _node: &PathPattern) -> TypecheckResult {
        todo!("check_path_pattern: Step 6")
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

// Compile-time keep-alive: silences unused-method warnings for stubs that
// will be wired up in later steps. Removed in Step 7 when `check_query`
// becomes public-facing.
#[allow(dead_code)]
fn _force_use(tc: &mut Typechecker, p: &PathPattern, e: &Expr, env: &TypeEnvironment) {
    let _ = tc.check_path_pattern(p);
    let _ = tc.check_expr(e, env);
    let _ = tc.refine_pattern_node(&None);
    let _ = tc.refine_pattern_edge(EdgeDir::Any, &None);
    let _ = tc.pow_path_type(&PathType::Zero, 0);
}
