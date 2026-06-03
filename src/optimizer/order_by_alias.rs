//! Resolve `ORDER BY <alias>` references to the underlying expression
//! when the alias maps to a pure `Expr::AttrLookup` in the RETURN list.
//!
//! The parser stores alias-keyed ORDER BY entries as
//! `SortKey::Column(idx)` where `idx` is the position of the matching
//! item in `RETURN`. The runtime treats any `Column` sort key as a
//! post-projection sort, and the `try_btree_ltj_real` precondition
//! requires `SortKey::Expr(AttrLookup)`. Both branches miss the
//! optimization opportunity even though the alias often points at a
//! plain attribute access — i.e. the sort can be evaluated directly
//! against the binding-table assignment without projecting first.
//!
//! This pass walks `Query.order_by` and, for each
//! `SortKey::Column(idx)` whose RETURN entry is a non-aggregate
//! `Expr::AttrLookup`, rewrites it to the corresponding
//! `SortKey::Expr`. Other shapes (aggregates, `COALESCE`, arithmetic,
//! aliased expressions over multiple variables) stay as `Column` —
//! their evaluation requires the projected row, so post-projection
//! sort is the only safe path.
//!
//! Effect on the IC2-style query
//! `RETURN msg.creationDate AS commentOrPostCreationDate
//!  ORDER BY commentOrPostCreationDate DESC, commentOrPostId ASC LIMIT 20`:
//! both alias references resolve to `AttrLookup` against `msg`, so the
//! sort key becomes pre-projection and the runtime's top-k heap +
//! pre-projection sort path projects only the surviving 20 rows
//! instead of all ~200 k. Cuts ~1 s on LDBC SF0.1.

use crate::syntax::expr::Expr;
use crate::syntax::query::{Query, ReturnItem, SortKey};

pub fn optimize(q: &mut Query) {
    // `sort_projected_rows` (the post-projection sort) requires every
    // spec to be `SortKey::Column`, and `sort_rows` (pre-projection)
    // requires every spec to be `SortKey::Expr`. The runtime picks
    // between them on `has_column_sort_key`. Producing a mixed list
    // would crash one of the two; rewrite all-or-nothing.
    //
    // Aggregate queries always go through the post-projection path
    // regardless of sort-key shape (the IR is `Vec<Vec<Value>>` after
    // aggregation, with no surviving binding-table to evaluate Expr
    // against). Bail out when any aggregate is present.
    let Some(specs) = &q.order_by else {
        return;
    };
    let Some(returns) = &q.returns else {
        return;
    };
    if returns.iter().any(|i| i.is_aggregate()) {
        return;
    }
    if q.group_by.is_some() {
        return;
    }
    let mut rewrites: Vec<Option<(String, String)>> = Vec::with_capacity(specs.len());
    for spec in specs {
        match &spec.key {
            SortKey::Expr(_) | SortKey::ColumnCast { .. } | SortKey::ColumnField { .. } => return,
            SortKey::Column(idx) => {
                let Some(ReturnItem::Expr {
                    expr: Expr::AttrLookup { var, attr },
                    ..
                }) = returns.get(*idx)
                else {
                    return; // any non-resolvable spec aborts the whole rewrite
                };
                rewrites.push(Some((var.clone(), attr.clone())));
            }
        }
    }

    let specs = q.order_by.as_mut().expect("checked above");
    for (spec, rw) in specs.iter_mut().zip(rewrites) {
        if let Some((var, attr)) = rw {
            spec.key = SortKey::Expr(Expr::AttrLookup { var, attr });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_query_unchecked;

    fn first_sort_key(input: &str) -> SortKey {
        let q = compile_query_unchecked(input).unwrap();
        q.order_by.unwrap().into_iter().next().unwrap().key
    }

    #[test]
    fn alias_to_attr_lookup_becomes_expr() {
        let key = first_sort_key("MATCH (n) RETURN n.x AS xx ORDER BY xx ASC");
        match key {
            SortKey::Expr(Expr::AttrLookup { var, attr }) => {
                assert_eq!(var, "n");
                assert_eq!(attr, "x");
            }
            other => panic!("expected SortKey::Expr(AttrLookup), got {other:?}"),
        }
    }

    #[test]
    fn alias_to_coalesce_stays_column() {
        let key = first_sort_key("MATCH (n) RETURN COALESCE(n.x, n.y) AS xy ORDER BY xy ASC");
        assert!(matches!(key, SortKey::Column(_)));
    }

    #[test]
    fn alias_to_aggregate_stays_column() {
        let key = first_sort_key("MATCH (n) RETURN COUNT(n) AS c GROUP BY n.x ORDER BY c DESC");
        assert!(matches!(key, SortKey::Column(_)));
    }

    #[test]
    fn alias_rewrite_aborts_when_mixed_with_casted_alias() {
        let q = compile_query_unchecked(
            "MATCH (n) RETURN n.x AS x, n.id AS id ORDER BY x ASC, CAST(id AS INTEGER) ASC",
        )
        .unwrap();
        let specs = q.order_by.unwrap();
        assert!(matches!(specs[0].key, SortKey::Column(_)));
        assert!(matches!(specs[1].key, SortKey::ColumnCast { .. }));
    }

    #[test]
    fn direct_expr_sort_unchanged() {
        let key = first_sort_key("MATCH (n) RETURN n.x ORDER BY n.x ASC");
        assert!(matches!(key, SortKey::Expr(Expr::AttrLookup { .. })));
    }
}
