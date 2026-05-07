//! Persistent catalog of named graph types.
//!
//! A `GraphTypeCatalog` is a session-scoped map of graph-type names to
//! `Schema` values, plus an "active" pointer that the typechecker consults
//! when compiling queries. The catalog is serialized to JSON and stored in
//! a chained page run inside the `.gdb`; the active name lives in the file
//! header. See `docs/internals/graph-type-catalog-plan.md` for the surface DDL
//! (`CREATE / USE / DROP GRAPH TYPE`) and the `DEFAULT` reservation rules.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::typing::variable_type::Schema;

/// Reserved name. Always represents the schema inferred from the current
/// graph data. Cannot be created or dropped via `CREATE` / `DROP`; lives
/// or dies with `USE GRAPH TYPE DEFAULT` and the import pipeline.
pub const DEFAULT_NAME: &str = "DEFAULT";

/// Catalog version tag. Bumped if the on-disk JSON shape ever changes
/// incompatibly. For now we only emit `1`; readers accept anything they
/// know how to deserialize.
pub const CATALOG_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphTypeCatalog {
    /// On-disk shape carries an explicit version field for forward compat.
    /// `Default::default()` initializes to `CATALOG_VERSION`.
    #[serde(default = "default_version")]
    pub version: u32,
    pub types: BTreeMap<String, Schema>,
    /// Active type name. Read from the file header on open; the in-memory
    /// catalog mirrors it so single-write paths work without re-reading
    /// the header.
    pub active: Option<String>,
    /// Cached validation verdicts, keyed by graph-type name. Populated
    /// by `VALIDATE GRAPH TYPE <name>`; cleared whenever the named type
    /// is replaced or dropped. Sidecar map (rather than nesting inside
    /// `types`) so old catalogs without validation data deserialize.
    #[serde(default)]
    pub validations: BTreeMap<String, ValidationStatus>,
    /// In-RAM only: set to `true` whenever the data underlying DEFAULT
    /// changes (i.e. after every successful DML). Cleared when the next
    /// access re-infers DEFAULT from the live store. Not serialized — a
    /// freshly opened `.gdb` always starts clean (the on-disk DEFAULT
    /// already reflects what's in the file).
    #[serde(skip)]
    pub default_dirty: bool,
}

/// Cached outcome of `VALIDATE GRAPH TYPE`. Lets re-USE of the same
/// type skip a second walk when the data hasn't changed. Mutation in a
/// later milestone must clear `validations` for affected names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationStatus {
    /// Total node count walked at validation time.
    pub against_node_count: u32,
    /// Total edge count walked at validation time.
    pub against_edge_count: u32,
    /// Number of element instances that fell outside every type in
    /// the schema. `0` means a clean pass.
    pub violations: u32,
    /// Wall-clock seconds since the Unix epoch at validation time.
    /// Display only; no logic depends on it.
    pub validated_at_unix: u64,
}

fn default_version() -> u32 {
    CATALOG_VERSION
}

impl GraphTypeCatalog {
    pub fn new() -> Self {
        GraphTypeCatalog {
            version: CATALOG_VERSION,
            types: BTreeMap::new(),
            active: None,
            validations: BTreeMap::new(),
            default_dirty: false,
        }
    }

    /// Flip the "DEFAULT needs re-inference" flag. Cheap: O(1). The
    /// runtime calls this after every successful DML; the next
    /// `SHOW GRAPH TYPE DEFAULT` / `USE GRAPH TYPE DEFAULT` /
    /// `VALIDATE GRAPH TYPE DEFAULT` triggers the lazy refresh.
    pub fn mark_default_dirty(&mut self) {
        self.default_dirty = true;
    }

    /// `true` iff DEFAULT is stale relative to the store. Set by
    /// `mark_default_dirty`, cleared by `install_default`.
    pub fn is_default_dirty(&self) -> bool {
        self.default_dirty
    }

    /// Register a user-defined graph type. Rejects the reserved DEFAULT
    /// name; CREATE for that name must go through `install_default`.
    /// Replacing an existing entry invalidates its cached validation.
    pub fn register(&mut self, name: String, schema: Schema) -> Result<(), String> {
        if is_default(&name) {
            return Err(format!(
                "{DEFAULT_NAME} is a reserved graph type name and cannot be created with CREATE GRAPH TYPE"
            ));
        }
        self.validations.remove(&name);
        self.types.insert(name, schema);
        Ok(())
    }

    /// Refresh and activate `DEFAULT`. Idempotent. Used by the import
    /// pipeline and by `USE GRAPH TYPE DEFAULT`. Drops any cached
    /// `DEFAULT` validation since the schema just changed.
    pub fn install_default(&mut self, schema: Schema) {
        self.validations.remove(DEFAULT_NAME);
        self.types.insert(DEFAULT_NAME.to_string(), schema);
        self.active = Some(DEFAULT_NAME.to_string());
        self.default_dirty = false;
    }

    /// Drop a graph type. Rejects DEFAULT. If the dropped name was active,
    /// the catalog reverts to "no active type" (callers fall back to
    /// `Schema::star()`).
    pub fn drop(&mut self, name: &str) -> Result<(), String> {
        if is_default(name) {
            return Err(format!(
                "{DEFAULT_NAME} is a reserved graph type and cannot be dropped"
            ));
        }
        if self.types.remove(name).is_none() {
            return Err(format!("graph type '{name}' not found"));
        }
        self.validations.remove(name);
        if self.active.as_deref() == Some(name) {
            self.active = None;
        }
        Ok(())
    }

    /// Record (or overwrite) a validation verdict for `name`.
    pub fn record_validation(&mut self, name: &str, status: ValidationStatus) {
        self.validations.insert(name.to_string(), status);
    }

    pub fn validation_for(&self, name: &str) -> Option<&ValidationStatus> {
        self.validations.get(name)
    }

    /// Activate an existing type. Plain `USE GRAPH TYPE name` (DEFAULT
    /// has its own refresh-and-activate path).
    pub fn set_active(&mut self, name: &str) -> Result<(), String> {
        if !self.types.contains_key(name) {
            return Err(format!("graph type '{name}' not found"));
        }
        self.active = Some(name.to_string());
        Ok(())
    }

    pub fn clear_active(&mut self) {
        self.active = None;
    }

    /// Resolve the active schema. Returns `Schema::star()` when nothing is
    /// active so the typechecker stays permissive instead of erroring.
    pub fn active_schema(&self) -> Schema {
        match &self.active {
            Some(name) => match self.types.get(name) {
                Some(s) => s.clone(),
                None => Schema::star(),
            },
            None => Schema::star(),
        }
    }

    pub fn active_name(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// `(name, is_active)` pairs. Sorted by name (BTreeMap order).
    pub fn list(&self) -> Vec<(&String, bool)> {
        let active = self.active.as_deref();
        self.types
            .keys()
            .map(|name| (name, Some(name.as_str()) == active))
            .collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.types.contains_key(name)
    }
}

fn is_default(name: &str) -> bool {
    name.eq_ignore_ascii_case(DEFAULT_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::variable_type::Schema;

    #[test]
    fn register_rejects_default() {
        let mut c = GraphTypeCatalog::new();
        assert!(c.register("DEFAULT".into(), Schema::star()).is_err());
        assert!(c.register("default".into(), Schema::star()).is_err());
    }

    #[test]
    fn drop_rejects_default() {
        let mut c = GraphTypeCatalog::new();
        c.install_default(Schema::star());
        assert!(c.drop("DEFAULT").is_err());
        assert!(c.drop("default").is_err());
        // still present
        assert!(c.contains("DEFAULT"));
    }

    #[test]
    fn install_default_sets_active() {
        let mut c = GraphTypeCatalog::new();
        c.install_default(Schema::star());
        assert_eq!(c.active_name(), Some("DEFAULT"));
        assert!(c.contains("DEFAULT"));
    }

    #[test]
    fn drop_clears_active_if_dropping_active() {
        let mut c = GraphTypeCatalog::new();
        c.register("strict".into(), Schema::star()).unwrap();
        c.set_active("strict").unwrap();
        c.drop("strict").unwrap();
        assert!(c.active_name().is_none());
    }

    #[test]
    fn drop_preserves_other_active() {
        let mut c = GraphTypeCatalog::new();
        c.register("a".into(), Schema::star()).unwrap();
        c.register("b".into(), Schema::star()).unwrap();
        c.set_active("a").unwrap();
        c.drop("b").unwrap();
        assert_eq!(c.active_name(), Some("a"));
    }

    #[test]
    fn set_active_unknown_errors() {
        let mut c = GraphTypeCatalog::new();
        assert!(c.set_active("ghost").is_err());
    }

    #[test]
    fn active_schema_falls_back_to_star_when_none() {
        let c = GraphTypeCatalog::new();
        let s = c.active_schema();
        assert!(!s.nodes.is_empty(), "star schema should have node types");
    }

    #[test]
    fn json_roundtrip_default_only() {
        let mut c = GraphTypeCatalog::new();
        c.install_default(Schema::star());
        let json = serde_json::to_string(&c).unwrap();
        let back: GraphTypeCatalog = serde_json::from_str(&json).unwrap();
        assert_eq!(back.active_name(), Some("DEFAULT"));
        assert!(back.contains("DEFAULT"));
        assert_eq!(back.version, CATALOG_VERSION);
    }
}
