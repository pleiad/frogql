//! Typechecker for gqlite path patterns and queries.
//!
//! Ported from `fppc/src/typechecker/checker.rs`. Public surface mirrors
//! fppc's `Typechecker` / `TypecheckResult`; the differences are documented
//! in `docs/typechecker_migration.md`.

use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::Expr;
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::Query;

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

    fn check_expr(&mut self, _e: &Expr, _env: &TypeEnvironment) -> SimpleType {
        todo!("check_expr: Step 4")
    }

    // -----------------------------------------------
    // Refinement helpers
    // -----------------------------------------------

    fn refine_pattern_node(&self, _desc: &Option<Descriptor>) -> VariableType {
        todo!("refine_pattern_node: Step 5")
    }

    fn refine_pattern_edge(
        &self,
        _dir: EdgeDir,
        _desc: &Option<Descriptor>,
    ) -> VariableType {
        todo!("refine_pattern_edge: Step 5")
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
