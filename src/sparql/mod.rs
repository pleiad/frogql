//! SPARQL frontend: a SPARQL 1.1 `SELECT` query becomes the same `Query`
//! AST the GQL parser produces, and from there runs through the ordinary
//! `elaborate → typecheck → optimize → run` pipeline. There is no second
//! engine.
//!
//! Parsing is `spargebra`'s (Oxigraph's SPARQL parser), which hands over
//! the SPARQL algebra with prefixes and IRIs already resolved. This module
//! is only the translation from that algebra to froGQL's AST.
//!
//! What it covers, what it rejects and why, is in
//! `docs/internals/sparql-frontend.md`. Everything it does not cover is a
//! translation error naming the construct, never a silent approximation.
//!
//! The one semantic choice made here rather than inherited: a SPARQL
//! variable denotes an RDF *term*, so in an expression or a projection it
//! reads the node's key property (`x.iri`, or `x.id` under
//! [`SparqlMapping::import_ttl`]), not the node itself. Equality on that
//! key is node identity because the key is unique.

use std::collections::{HashMap, HashSet};

use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern, OrderExpression,
    PropertyPathExpression as Path,
};
use spargebra::term::{Literal, NamedNode, NamedNodePattern, TermPattern, TriplePattern, Variable};
use spargebra::{Query as SparqlQuery, SparqlParser};

use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::path_prefix::{PathMode, PathPrefix, PathSearch};
use crate::syntax::query::{
    Aggregator, GeneralSetKind, MatchStatement, NullsOrder, Query, ReturnItem, SetQuantifier,
    SortDir, SortKey, SortSpec,
};
use crate::typing::descriptor_type::DescriptorType;
use crate::typing::label_type::LabelType;
use crate::typing::property_type::PropertyType;

/// How RDF terms are laid out in the property graph a query runs against.
/// The translation needs it because the graph does not say: an RDF import
/// can store an IRI whole or keep only its local name, and a predicate can
/// become an edge label either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlMapping {
    /// How a predicate IRI becomes an edge label.
    pub predicates: PredicateNaming,
    /// Which node property identifies a resource.
    pub node_key: NodeKey,
    /// The label every resource node carries, when there is one. A
    /// constant IRI is then a point lookup in the `(label, key)` index;
    /// without a label the engine has no index to ask and scans.
    pub node_label: Option<String>,
}

/// How a predicate IRI becomes an edge label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateNaming {
    /// The label is the whole IRI.
    FullIri,
    /// The label is the IRI's local name (`…/P121` → `P121`).
    LocalName,
}

/// Which node property identifies a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKey {
    /// A string property holding the whole IRI.
    Iri { prop: String },
    /// An integer property holding the IRI's local name, which is what
    /// `import_ttl` writes (`img:1164163` → `id = 1164163`).
    LocalNameInt { prop: String },
}

impl Default for SparqlMapping {
    /// Every IRI kept whole: predicates are edge labels, resources carry
    /// an `iri` string property, and there is no common node label.
    fn default() -> Self {
        SparqlMapping {
            predicates: PredicateNaming::FullIri,
            node_key: NodeKey::Iri { prop: "iri".into() },
            node_label: None,
        }
    }
}

impl SparqlMapping {
    /// The layout `import_ttl` writes: every node carries `label` and an
    /// integer `id_prop` holding its local name, and an edge label is the
    /// predicate's local name. `import_ttl`'s defaults are `("img", "id")`.
    pub fn import_ttl(label: &str, id_prop: &str) -> Self {
        SparqlMapping {
            predicates: PredicateNaming::LocalName,
            node_key: NodeKey::LocalNameInt {
                prop: id_prop.into(),
            },
            node_label: Some(label.into()),
        }
    }

    fn key_prop(&self) -> &str {
        match &self.node_key {
            NodeKey::Iri { prop } | NodeKey::LocalNameInt { prop } => prop,
        }
    }

    /// The key value a node standing for `iri` holds. `Null` when the IRI
    /// has no integer local name under `LocalNameInt`: no node can stand
    /// for it, and a comparison with null keeps no row.
    fn key_value(&self, iri: &NamedNode) -> Value {
        match &self.node_key {
            NodeKey::Iri { .. } => Value::Str(iri.as_str().to_string()),
            NodeKey::LocalNameInt { .. } => local_name(iri.as_str())
                .parse::<i64>()
                .map_or(Value::Null, Value::Int),
        }
    }

    fn edge_label(&self, iri: &NamedNode) -> LabelType {
        LabelType::Label(match self.predicates {
            PredicateNaming::FullIri => iri.as_str().to_string(),
            PredicateNaming::LocalName => local_name(iri.as_str()).to_string(),
        })
    }
}

/// The local name of an IRI: what follows its last `/`, `#` or `:`. The
/// same split `import_ttl` applies to a prefixed name.
pub fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#', ':']).next().unwrap_or(iri)
}

/// Translate a SPARQL `SELECT` query to an unchecked, unelaborated
/// `Query`, as `parser::parse_query` would return it.
pub fn translate(input: &str, mapping: &SparqlMapping) -> Result<Query, String> {
    let parsed = SparqlParser::new()
        .parse_query(input)
        .map_err(|e| format!("SPARQL syntax error: {e}"))?;
    let pattern = match parsed {
        SparqlQuery::Select {
            dataset, pattern, ..
        } => {
            if dataset.is_some() {
                return Err("FROM / FROM NAMED are not supported".into());
            }
            pattern
        }
        SparqlQuery::Ask { .. } => {
            return Err("ASK is not supported yet; SELECT ... LIMIT 1 asks the same".into())
        }
        SparqlQuery::Construct { .. } => return Err("CONSTRUCT is not supported".into()),
        SparqlQuery::Describe { .. } => return Err("DESCRIBE is not supported".into()),
    };
    let mut t = Translator::new(input, mapping);
    count_pattern_uses(&pattern, &mut t.uses);
    t.select(&pattern)
}

struct Translator<'m> {
    mapping: &'m SparqlMapping,
    /// Prefix of every variable the translation invents. Chosen so that
    /// it occurs nowhere in the input, so no user variable can carry it.
    hidden: String,
    fresh: usize,
    /// One hidden variable per distinct constant IRI, so two mentions of
    /// the same resource are one node to the join.
    constants: HashMap<String, String>,
    /// How many times each variable is mentioned anywhere in the query.
    uses: HashMap<String, usize>,
    /// Aggregate variables `spargebra` introduces, by name.
    aggregates: HashMap<String, Aggregator>,
    /// Variables bound by `SELECT (expr AS ?v)` or a trailing `BIND`.
    computed: HashMap<String, Expr>,
}

impl<'m> Translator<'m> {
    fn new(input: &str, mapping: &'m SparqlMapping) -> Self {
        let mut hidden = String::from("_h");
        while input.contains(&hidden) {
            hidden.insert(0, '_');
        }
        Translator {
            mapping,
            hidden,
            fresh: 0,
            constants: HashMap::new(),
            uses: HashMap::new(),
            aggregates: HashMap::new(),
            computed: HashMap::new(),
        }
    }

    fn fresh_var(&mut self, kind: &str) -> String {
        self.fresh += 1;
        format!("{}{}{}", self.hidden, kind, self.fresh)
    }

    // ---- solution modifiers ----

    /// `spargebra` nests a SELECT as
    /// `Slice(Distinct(Project(OrderBy(Extend*(Group(where))))))`, each
    /// layer optional except `Project`. Peel them in that order.
    fn select(&mut self, root: &GraphPattern) -> Result<Query, String> {
        let mut gp = root;

        let mut limit = None;
        if let GraphPattern::Slice {
            inner,
            start,
            length,
        } = gp
        {
            if *start > 0 {
                return Err("OFFSET is not supported: froGQL has no OFFSET".into());
            }
            limit = length.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
            gp = inner;
        }

        let mut distinct = false;
        match gp {
            GraphPattern::Distinct { inner } => {
                distinct = true;
                gp = inner;
            }
            // REDUCED permits but does not require eliminating duplicates.
            GraphPattern::Reduced { inner } => gp = inner,
            _ => {}
        }

        let GraphPattern::Project { inner, variables } = gp else {
            return Err("internal: a SELECT without a projection".into());
        };
        let projected = variables;
        gp = inner;

        let mut order = None;
        if let GraphPattern::OrderBy { inner, expression } = gp {
            order = Some(expression);
            gp = inner;
        }

        let mut extends = Vec::new();
        while let GraphPattern::Extend {
            inner,
            variable,
            expression,
        } = gp
        {
            extends.push((variable, expression));
            gp = inner;
        }
        // Outermost first as peeled; bind innermost first so a later
        // expression can name an earlier one.
        extends.reverse();

        if let GraphPattern::Filter { inner, .. } = gp {
            if matches!(**inner, GraphPattern::Group { .. }) {
                return Err("HAVING is not supported yet".into());
            }
        }

        let mut group_by = None;
        if let GraphPattern::Group {
            inner,
            variables,
            aggregates,
        } = gp
        {
            for (v, agg) in aggregates {
                let a = self.aggregate(agg)?;
                self.aggregates.insert(v.as_str().to_string(), a);
            }
            if !variables.is_empty() {
                group_by = Some(
                    variables
                        .iter()
                        .map(|v| Expr::Var(v.as_str().to_string()))
                        .collect(),
                );
            }
            gp = inner;
            if matches!(gp, GraphPattern::Extend { .. }) {
                return Err("GROUP BY an expression is not supported yet".into());
            }
        }

        let in_scope = in_scope_vars(gp);
        let matches = self.clauses(gp)?;
        if matches.is_empty() {
            return Err("an empty WHERE clause is not supported".into());
        }

        for (v, e) in extends {
            let x = self.expr(e)?;
            self.computed.insert(v.as_str().to_string(), x);
        }

        let mut returns = Vec::new();
        for v in projected {
            let name = v.as_str();
            let expr = if self.computed.contains_key(name) || in_scope.contains(name) {
                self.var_value(name)
            } else {
                // SPARQL projects a variable the pattern never binds as an
                // all-unbound column.
                Expr::Const(Value::Null)
            };
            returns.push(ReturnItem::Expr {
                expr,
                alias: Some(name.to_string()),
            });
        }

        let order_by = match order {
            None => None,
            Some(exprs) => Some(self.order(exprs, &returns)?),
        };

        Ok(Query {
            matches,
            group_by,
            returns: Some(returns),
            distinct,
            order_by,
            limit,
            ..Query::empty()
        })
    }

    fn order(
        &mut self,
        exprs: &[OrderExpression],
        returns: &[ReturnItem],
    ) -> Result<Vec<SortSpec>, String> {
        let mut specs = Vec::new();
        for oe in exprs {
            let (e, dir) = match oe {
                OrderExpression::Asc(e) => (e, SortDir::Asc),
                OrderExpression::Desc(e) => (e, SortDir::Desc),
            };
            let translated = self.expr(e)?;
            // A key that is also a projected column sorts by that column,
            // after projection: the only way to order by an aggregate.
            let column = returns
                .iter()
                .position(|r| matches!(r, ReturnItem::Expr { expr, .. } if *expr == translated));
            let key = match column {
                Some(idx) => SortKey::Column(idx),
                None => SortKey::Expr(translated),
            };
            // SPARQL ranks an unbound value below every bound one.
            let nulls = Some(match dir {
                SortDir::Asc => NullsOrder::First,
                SortDir::Desc => NullsOrder::Last,
            });
            specs.push(SortSpec { key, dir, nulls });
        }
        Ok(specs)
    }

    fn aggregate(&mut self, a: &AggregateExpression) -> Result<Aggregator, String> {
        match a {
            AggregateExpression::CountSolutions { distinct: false } => Ok(Aggregator::CountStar),
            AggregateExpression::CountSolutions { distinct: true } => {
                Err("COUNT(DISTINCT *) is not supported yet".into())
            }
            AggregateExpression::FunctionCall {
                name,
                expr,
                distinct,
            } => {
                let kind = match name {
                    AggregateFunction::Count => GeneralSetKind::Count,
                    AggregateFunction::Sum => GeneralSetKind::Sum,
                    AggregateFunction::Avg => GeneralSetKind::Avg,
                    AggregateFunction::Min => GeneralSetKind::Min,
                    AggregateFunction::Max => GeneralSetKind::Max,
                    // SAMPLE may return any value of the group; the
                    // minimum is one.
                    AggregateFunction::Sample => GeneralSetKind::Min,
                    other => return Err(format!("aggregate {other} is not supported yet")),
                };
                Ok(Aggregator::GeneralSet {
                    kind,
                    quantifier: if *distinct {
                        SetQuantifier::Distinct
                    } else {
                        SetQuantifier::All
                    },
                    expr: self.expr(expr)?,
                })
            }
        }
    }

    // ---- graph patterns ----

    /// The MATCH chain for one group graph pattern. Adjacent plain clauses
    /// are merged into one comma-join so the LTJ sees them together.
    fn clauses(&mut self, gp: &GraphPattern) -> Result<Vec<MatchStatement>, String> {
        let out = match gp {
            GraphPattern::Bgp { patterns } => {
                let mut triples = Vec::new();
                for t in patterns {
                    triples.push(self.triple(t)?);
                }
                match join_all(triples) {
                    Some(pattern) => vec![MatchStatement::Simple { pattern }],
                    None => Vec::new(),
                }
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => vec![MatchStatement::Simple {
                pattern: self.path(subject, path, object)?,
            }],
            GraphPattern::Join { left, right } => {
                let mut a = self.clauses(left)?;
                a.extend(self.clauses(right)?);
                a
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                let mut a = self.clauses(left)?;
                let b = self.clauses(right)?;
                let mut pattern = collapse(b, "OPTIONAL")?;
                if let Some(e) = expression {
                    pattern = PathPattern::Filter(Box::new(pattern), self.expr(e)?);
                }
                a.push(MatchStatement::Optional { pattern });
                a
            }
            GraphPattern::Filter { expr, inner } => {
                let mut cl = self.clauses(inner)?;
                let e = self.expr(expr)?;
                attach_filter(&mut cl, e, expr, inner)?;
                cl
            }
            GraphPattern::Union { left, right } => {
                if in_scope_vars(left) != in_scope_vars(right) {
                    return Err(
                        "UNION branches that bind different variables are not supported yet".into(),
                    );
                }
                let a = self.clauses(left)?;
                let a = collapse(a, "UNION")?;
                let b = self.clauses(right)?;
                let b = collapse(b, "UNION")?;
                vec![MatchStatement::Simple {
                    pattern: PathPattern::Union(Box::new(a), Box::new(b)),
                }]
            }
            GraphPattern::Extend { .. } => return Err("BIND is not supported yet".into()),
            GraphPattern::Minus { .. } => return Err("MINUS is not supported yet".into()),
            GraphPattern::Values { .. } => return Err("VALUES is not supported yet".into()),
            GraphPattern::Graph { .. } => return Err("GRAPH is not supported".into()),
            GraphPattern::Service { .. } => return Err("SERVICE is not supported".into()),
            _ => return Err("subqueries are not supported yet".into()),
        };
        Ok(merge_simple(out))
    }

    fn triple(&mut self, t: &TriplePattern) -> Result<PathPattern, String> {
        let s = self.node(&t.subject)?;
        let o = self.node(&t.object)?;
        let edge = match &t.predicate {
            NamedNodePattern::NamedNode(p) => edge(Some(self.mapping.edge_label(p)), true),
            NamedNodePattern::Variable(v) => {
                // A predicate variable mentioned once is a wildcard. Mentioned
                // again — shared with another triple, projected, filtered —
                // it would have to denote the label, which the AST cannot
                // express yet (docs/internals/sparql-frontend.md, phase 2).
                if self.uses.get(v.as_str()).copied().unwrap_or(0) > 1 {
                    return Err(format!(
                        "the predicate variable ?{} is used more than once; \
                         variable predicates are not supported yet beyond a wildcard",
                        v.as_str()
                    ));
                }
                edge(None, true)
            }
        };
        Ok(concat3(s, edge, o))
    }

    fn node(&mut self, term: &TermPattern) -> Result<PathPattern, String> {
        let desc = match term {
            TermPattern::Variable(v) => {
                Descriptor::new(Some(v.as_str().to_string()), DescriptorType::star())
            }
            TermPattern::BlankNode(b) => {
                let var = format!("{}b{}", self.hidden, b.as_str());
                Descriptor::new(Some(var), DescriptorType::star())
            }
            TermPattern::NamedNode(iri) => {
                let var = match self.constants.get(iri.as_str()) {
                    Some(v) => v.clone(),
                    None => {
                        let v = self.fresh_var("c");
                        self.constants.insert(iri.as_str().to_string(), v.clone());
                        v
                    }
                };
                let label = self
                    .mapping
                    .node_label
                    .as_ref()
                    .map_or(LabelType::Star, |l| LabelType::Label(l.clone()));
                // The key filter goes on every mention, not just the
                // first: a mention inside an OPTIONAL is evaluated on its
                // own and must not range over every node.
                Descriptor::with_filters(
                    Some(var),
                    DescriptorType::new(label, PropertyType::open_empty()),
                    vec![(
                        self.mapping.key_prop().to_string(),
                        Expr::Const(self.mapping.key_value(iri)),
                    )],
                )
            }
            TermPattern::Literal(_) => {
                return Err("literals in triple patterns are not supported yet \
                     (literals are not imported; see docs/internals/sparql-frontend.md)"
                    .into())
            }
            #[allow(unreachable_patterns)]
            _ => return Err("RDF-star triple terms are not supported".into()),
        };
        Ok(PathPattern::Node(Some(desc)))
    }

    /// A property path between two terms.
    ///
    /// `*`, `+` and `?` evaluate to one solution per connected pair in
    /// SPARQL, however many paths connect it. `ANY SHORTEST` keeps one path
    /// per `(first, last)` partition, which is the same thing, and it is
    /// what licenses an unbounded repetition. It must cover the closure and
    /// nothing else — around a sequence it would also collapse the
    /// multiplicity of the plain steps — so a sequence holding a closure is
    /// split into joined operands at a hidden middle node, which is how
    /// SPARQL itself defines a sequence.
    fn path(&mut self, s: &TermPattern, p: &Path, o: &TermPattern) -> Result<PathPattern, String> {
        if !has_closure(p) {
            let sn = self.node(s)?;
            let seg = self.segment(p, true)?;
            let on = self.node(o)?;
            return Ok(concat3(sn, seg, on));
        }
        match p {
            Path::Reverse(inner) => self.path(o, inner, s),
            Path::Sequence(a, b) => {
                let mid = TermPattern::Variable(Variable::new_unchecked(self.fresh_var("m")));
                let left = self.path(s, a, &mid)?;
                let right = self.path(&mid, b, o)?;
                Ok(PathPattern::Join(Box::new(left), Box::new(right)))
            }
            Path::ZeroOrMore(inner) | Path::OneOrMore(inner) | Path::ZeroOrOne(inner)
                if !has_closure(inner) =>
            {
                let (lb, ub) = match p {
                    Path::ZeroOrMore(_) => (0, None),
                    Path::OneOrMore(_) => (1, None),
                    _ => (0, Some(1)),
                };
                let sn = self.node(s)?;
                let body = self.segment(inner, true)?;
                let on = self.node(o)?;
                let repeat = PathPattern::Repeat {
                    pattern: Box::new(body),
                    lb,
                    ub,
                };
                Ok(PathPattern::Selected {
                    prefix: PathPrefix {
                        mode: PathMode::Walk,
                        search: PathSearch::ShortestPaths { count: 1 },
                    },
                    pattern: Box::new(concat3(sn, repeat, on)),
                })
            }
            _ => Err(
                "this property path is not supported yet: a `*`, `+` or `?` \
                 inside an alternative or inside another `*`, `+` or `?`"
                    .into(),
            ),
        }
    }

    /// A closure-free path as edges and anonymous nodes, without its two
    /// endpoints. `forward` is false under an odd number of `^`.
    fn segment(&mut self, p: &Path, forward: bool) -> Result<PathPattern, String> {
        if let Some((label, fwd)) = self.edge_only(p, forward) {
            return Ok(edge(Some(label), fwd));
        }
        match p {
            Path::Reverse(inner) => self.segment(inner, !forward),
            Path::Sequence(a, b) => {
                let (first, second) = if forward { (a, b) } else { (b, a) };
                let x = self.segment(first, forward)?;
                let y = self.segment(second, forward)?;
                Ok(concat3(x, anonymous_node(), y))
            }
            Path::Alternative(a, b) => {
                let x = self.segment(a, forward)?;
                let y = self.segment(b, forward)?;
                Ok(PathPattern::Union(Box::new(x), Box::new(y)))
            }
            _ => Err("internal: a closure reached segment()".into()),
        }
    }

    /// A path that is one edge — a predicate, a negated set, or an
    /// alternative of those in one direction — as its label expression and
    /// direction. An alternative of single edges is then one edge with a
    /// label disjunction instead of a union of two patterns.
    fn edge_only(&self, p: &Path, forward: bool) -> Option<(LabelType, bool)> {
        match p {
            Path::NamedNode(n) => Some((self.mapping.edge_label(n), forward)),
            Path::Reverse(inner) => self.edge_only(inner, !forward),
            Path::NegatedPropertySet(ns) => {
                let set = ns
                    .iter()
                    .map(|n| self.mapping.edge_label(n))
                    .reduce(|a, b| LabelType::Or(Box::new(a), Box::new(b)))?;
                Some((LabelType::Neg(Box::new(set)), forward))
            }
            Path::Alternative(a, b) => {
                let (la, da) = self.edge_only(a, forward)?;
                let (lb, db) = self.edge_only(b, forward)?;
                (da == db).then(|| (LabelType::Or(Box::new(la), Box::new(lb)), da))
            }
            _ => None,
        }
    }

    // ---- expressions ----

    /// The value a variable denotes: an aggregate, a computed expression,
    /// or the key of the node it is bound to.
    fn var_value(&self, name: &str) -> Expr {
        if let Some(a) = self.aggregates.get(name) {
            return Expr::Agg(Box::new(a.clone()));
        }
        if let Some(e) = self.computed.get(name) {
            return e.clone();
        }
        Expr::AttrLookup {
            var: name.to_string(),
            attr: self.mapping.key_prop().to_string(),
        }
    }

    fn expr(&mut self, e: &Expression) -> Result<Expr, String> {
        Ok(match e {
            Expression::NamedNode(n) => Expr::Const(self.mapping.key_value(n)),
            Expression::Literal(l) => Expr::Const(literal(l)?),
            Expression::Variable(v) => self.var_value(v.as_str()),
            Expression::Or(a, b) => self.binop(BinOp::Or, a, b)?,
            Expression::And(a, b) => self.binop(BinOp::And, a, b)?,
            Expression::Equal(a, b) | Expression::SameTerm(a, b) => self.binop(BinOp::Eq, a, b)?,
            Expression::Greater(a, b) => self.binop(BinOp::Gt, a, b)?,
            Expression::GreaterOrEqual(a, b) => self.binop(BinOp::Ge, a, b)?,
            Expression::Less(a, b) => self.binop(BinOp::Lt, a, b)?,
            Expression::LessOrEqual(a, b) => self.binop(BinOp::Le, a, b)?,
            Expression::Add(a, b) => self.binop(BinOp::Add, a, b)?,
            Expression::Subtract(a, b) => self.binop(BinOp::Sub, a, b)?,
            Expression::Multiply(a, b) => self.binop(BinOp::Mul, a, b)?,
            Expression::Divide(a, b) => self.binop(BinOp::Div, a, b)?,
            Expression::In(a, list) => {
                // An OR chain of equalities: froGQL's `IN` takes a list,
                // and a list literal holds constants only.
                let lhs = self.expr(a)?;
                let mut acc: Option<Expr> = None;
                for item in list {
                    let eq = bin(BinOp::Eq, lhs.clone(), self.expr(item)?);
                    acc = Some(match acc {
                        None => eq,
                        Some(prev) => bin(BinOp::Or, prev, eq),
                    });
                }
                acc.unwrap_or(Expr::Const(Value::Bool(false)))
            }
            Expression::UnaryPlus(a) => self.expr(a)?,
            Expression::UnaryMinus(a) => Expr::Unop {
                op: UnOp::Neg,
                operand: Box::new(self.expr(a)?),
            },
            Expression::Not(inner) => match &**inner {
                Expression::Exists(p) => Expr::NotExists {
                    body: Box::new(self.subquery(p)?),
                },
                other => Expr::Unop {
                    op: UnOp::Not,
                    operand: Box::new(self.expr(other)?),
                },
            },
            Expression::Exists(p) => Expr::Exists {
                body: Box::new(self.subquery(p)?),
            },
            Expression::Bound(v) => {
                let operand = match self.computed.get(v.as_str()) {
                    Some(e) => e.clone(),
                    None => Expr::Var(v.as_str().to_string()),
                };
                Expr::IsNull {
                    operand: Box::new(operand),
                    negated: true,
                }
            }
            Expression::If(c, a, b) => Expr::Case {
                branches: vec![(self.expr(c)?, self.expr(a)?)],
                else_expr: Some(Box::new(self.expr(b)?)),
            },
            Expression::Coalesce(xs) => {
                let mut args = Vec::new();
                for x in xs {
                    args.push(self.expr(x)?);
                }
                match args.len() {
                    0 => Expr::Const(Value::Null),
                    1 => args.pop().expect("one argument"),
                    _ => Expr::Coalesce(args),
                }
            }
            Expression::FunctionCall(f, _) => {
                return Err(format!("SPARQL function {f} is not supported yet"))
            }
        })
    }

    fn binop(&mut self, op: BinOp, a: &Expression, b: &Expression) -> Result<Expr, String> {
        Ok(bin(op, self.expr(a)?, self.expr(b)?))
    }

    /// The body of `EXISTS { … }`: a MATCH chain, nothing else.
    fn subquery(&mut self, p: &GraphPattern) -> Result<Query, String> {
        let matches = self.clauses(p)?;
        if matches.is_empty() {
            return Err("an empty EXISTS pattern is not supported".into());
        }
        Ok(Query {
            matches,
            ..Query::empty()
        })
    }
}

// ---- AST helpers ----

fn bin(op: BinOp, left: Expr, right: Expr) -> Expr {
    Expr::Binop {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn concat3(a: PathPattern, b: PathPattern, c: PathPattern) -> PathPattern {
    PathPattern::Concat(
        Box::new(PathPattern::Concat(Box::new(a), Box::new(b))),
        Box::new(c),
    )
}

fn edge(label: Option<LabelType>, forward: bool) -> PathPattern {
    let dtype = match label {
        Some(l) => DescriptorType::new(l, PropertyType::open_empty()),
        None => DescriptorType::star(),
    };
    let desc = Some(Descriptor::new(None, dtype));
    if forward {
        PathPattern::EdgeRight(desc)
    } else {
        PathPattern::EdgeLeft(desc)
    }
}

/// `()` as the parser builds it.
fn anonymous_node() -> PathPattern {
    PathPattern::Node(Some(Descriptor::new(None, DescriptorType::star())))
}

fn join_all(parts: Vec<PathPattern>) -> Option<PathPattern> {
    parts
        .into_iter()
        .reduce(|a, b| PathPattern::Join(Box::new(a), Box::new(b)))
}

/// Merge each run of adjacent plain clauses into one comma-join.
fn merge_simple(clauses: Vec<MatchStatement>) -> Vec<MatchStatement> {
    let mut out: Vec<MatchStatement> = Vec::new();
    for c in clauses {
        match (out.last_mut(), c) {
            (
                Some(MatchStatement::Simple { pattern: prev }),
                MatchStatement::Simple { pattern },
            ) => {
                let left = std::mem::replace(prev, anonymous_node());
                *prev = PathPattern::Join(Box::new(left), Box::new(pattern));
            }
            (_, c) => out.push(c),
        }
    }
    out
}

/// A group that must be one pattern (an OPTIONAL body, a UNION branch).
fn collapse(clauses: Vec<MatchStatement>, what: &str) -> Result<PathPattern, String> {
    let mut patterns = Vec::new();
    for c in clauses {
        match c {
            MatchStatement::Simple { pattern } => patterns.push(pattern),
            MatchStatement::Optional { .. } => {
                return Err(format!("an OPTIONAL inside {what} is not supported yet"))
            }
        }
    }
    join_all(patterns).ok_or_else(|| format!("an empty {what} group is not supported"))
}

/// Put a FILTER on the clause that evaluates it.
///
/// A clause's WHERE belongs to that clause, so a filter can go on a plain
/// clause only if it reads nothing an OPTIONAL after it binds: on such
/// variables it filters the left join's result, which no clause of the
/// chain expresses. A filter over the required part alone commutes with
/// the left join and goes on the first clause.
fn attach_filter(
    clauses: &mut Vec<MatchStatement>,
    e: Expr,
    sparql: &Expression,
    inner: &GraphPattern,
) -> Result<(), String> {
    if clauses.iter().all(|c| !c.is_optional()) {
        let pattern = collapse(std::mem::take(clauses), "FILTER")?;
        clauses.push(MatchStatement::Simple {
            pattern: PathPattern::Filter(Box::new(pattern), e),
        });
        return Ok(());
    }
    let shape_ok = matches!(clauses.first(), Some(MatchStatement::Simple { .. }))
        && clauses[1..].iter().all(|c| c.is_optional());
    let required = in_scope_vars(required_part(inner));
    let optional_only: HashSet<String> = in_scope_vars(inner)
        .difference(&required)
        .cloned()
        .collect();
    let mut mentioned = HashMap::new();
    count_expr_uses(sparql, &mut mentioned);
    if !shape_ok || mentioned.keys().any(|v| optional_only.contains(v)) {
        return Err("a FILTER over a variable bound by OPTIONAL is not supported yet".into());
    }
    if let Some(MatchStatement::Simple { pattern }) = clauses.first_mut() {
        let p = std::mem::replace(pattern, anonymous_node());
        *pattern = PathPattern::Filter(Box::new(p), e);
    }
    Ok(())
}

/// The part of a group every solution binds: the left of its left joins.
fn required_part(mut gp: &GraphPattern) -> &GraphPattern {
    while let GraphPattern::LeftJoin { left, .. } = gp {
        gp = left;
    }
    gp
}

fn in_scope_vars(gp: &GraphPattern) -> HashSet<String> {
    let mut out = HashSet::new();
    gp.on_in_scope_variable(|v| {
        out.insert(v.as_str().to_string());
    });
    out
}

fn has_closure(p: &Path) -> bool {
    match p {
        Path::ZeroOrMore(_) | Path::OneOrMore(_) | Path::ZeroOrOne(_) => true,
        Path::Reverse(i) => has_closure(i),
        Path::Sequence(a, b) | Path::Alternative(a, b) => has_closure(a) || has_closure(b),
        Path::NamedNode(_) | Path::NegatedPropertySet(_) => false,
    }
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

fn literal(l: &Literal) -> Result<Value, String> {
    if l.language().is_some() {
        return Err("language-tagged literals are not supported yet".into());
    }
    let dt = l.datatype().as_str();
    let v = l.value();
    let bad = || format!("invalid literal \"{v}\" for <{dt}>");
    match dt.strip_prefix(XSD) {
        Some("string") => Ok(Value::Str(v.to_string())),
        Some("boolean") => match v {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(bad()),
        },
        Some(
            "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
            | "positiveInteger" | "negativeInteger" | "nonPositiveInteger" | "unsignedInt"
            | "unsignedLong" | "unsignedShort" | "unsignedByte",
        ) => v.parse::<i64>().map(Value::Int).map_err(|_| bad()),
        Some("decimal" | "double" | "float") => {
            v.parse::<f64>().map(Value::Float).map_err(|_| bad())
        }
        _ => Err(format!("literals of type <{dt}> are not supported yet")),
    }
}

// ---- variable use counts ----
//
// A predicate variable is translatable as a wildcard only when nothing
// else mentions it, which needs the whole query's mentions up front.

fn bump(uses: &mut HashMap<String, usize>, v: &Variable) {
    *uses.entry(v.as_str().to_string()).or_default() += 1;
}

fn count_term_uses(t: &TermPattern, uses: &mut HashMap<String, usize>) {
    if let TermPattern::Variable(v) = t {
        bump(uses, v);
    }
}

fn count_pattern_uses(gp: &GraphPattern, uses: &mut HashMap<String, usize>) {
    match gp {
        GraphPattern::Bgp { patterns } => {
            for t in patterns {
                count_term_uses(&t.subject, uses);
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    bump(uses, v);
                }
                count_term_uses(&t.object, uses);
            }
        }
        GraphPattern::Path {
            subject, object, ..
        } => {
            count_term_uses(subject, uses);
            count_term_uses(object, uses);
        }
        GraphPattern::Join { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => {
            count_pattern_uses(left, uses);
            count_pattern_uses(right, uses);
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            count_pattern_uses(left, uses);
            count_pattern_uses(right, uses);
            if let Some(e) = expression {
                count_expr_uses(e, uses);
            }
        }
        GraphPattern::Filter { expr, inner } => {
            count_expr_uses(expr, uses);
            count_pattern_uses(inner, uses);
        }
        GraphPattern::Graph { name, inner } | GraphPattern::Service { name, inner, .. } => {
            if let NamedNodePattern::Variable(v) = name {
                bump(uses, v);
            }
            count_pattern_uses(inner, uses);
        }
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } => {
            bump(uses, variable);
            count_expr_uses(expression, uses);
            count_pattern_uses(inner, uses);
        }
        GraphPattern::Values { variables, .. } => {
            for v in variables {
                bump(uses, v);
            }
        }
        GraphPattern::OrderBy { inner, expression } => {
            for oe in expression {
                match oe {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => count_expr_uses(e, uses),
                }
            }
            count_pattern_uses(inner, uses);
        }
        GraphPattern::Project { inner, variables } => {
            for v in variables {
                bump(uses, v);
            }
            count_pattern_uses(inner, uses);
        }
        GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => count_pattern_uses(inner, uses),
        GraphPattern::Group {
            inner,
            variables,
            aggregates,
        } => {
            for v in variables {
                bump(uses, v);
            }
            for (_, a) in aggregates {
                if let AggregateExpression::FunctionCall { expr, .. } = a {
                    count_expr_uses(expr, uses);
                }
            }
            count_pattern_uses(inner, uses);
        }
        #[allow(unreachable_patterns)]
        _ => {}
    }
}

fn count_expr_uses(e: &Expression, uses: &mut HashMap<String, usize>) {
    match e {
        Expression::NamedNode(_) | Expression::Literal(_) => {}
        Expression::Variable(v) | Expression::Bound(v) => bump(uses, v),
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            count_expr_uses(a, uses);
            count_expr_uses(b, uses);
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            count_expr_uses(a, uses)
        }
        Expression::In(a, list) => {
            count_expr_uses(a, uses);
            for x in list {
                count_expr_uses(x, uses);
            }
        }
        Expression::Exists(p) => count_pattern_uses(p, uses),
        Expression::If(a, b, c) => {
            count_expr_uses(a, uses);
            count_expr_uses(b, uses);
            count_expr_uses(c, uses);
        }
        Expression::Coalesce(xs) | Expression::FunctionCall(_, xs) => {
            for x in xs {
                count_expr_uses(x, uses);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_name_splits_on_the_last_separator() {
        assert_eq!(local_name("http://ex.org/a/P121"), "P121");
        assert_eq!(local_name("http://ex.org/onto#knows"), "knows");
        assert_eq!(local_name("img:1164163"), "1164163");
        assert_eq!(local_name("plain"), "plain");
    }

    #[test]
    fn hidden_prefix_avoids_the_input() {
        let m = SparqlMapping::default();
        let q = "SELECT ?_h1 WHERE { ?_h1 <p> ?x }";
        let t = Translator::new(q, &m);
        assert!(!q.contains(&t.hidden));
    }

    #[test]
    fn unsupported_constructs_name_themselves() {
        let m = SparqlMapping::default();
        let err = |q: &str| translate(&format!("PREFIX e: <http://e/> {q}"), &m).unwrap_err();
        let cases = [
            ("SELECT * WHERE { ?s e:p ?o } OFFSET 5", "OFFSET"),
            ("ASK { ?s e:p ?o }", "ASK"),
            ("SELECT * WHERE { ?s e:p \"x\" }", "literals"),
            ("SELECT ?p WHERE { ?s ?p ?o }", "predicate variable"),
            ("SELECT * WHERE { ?s e:p ?o MINUS { ?s e:q ?o } }", "MINUS"),
            ("SELECT * WHERE { ?s (e:p|e:q+) ?o }", "property path"),
        ];
        for (q, want) in cases {
            let e = err(q);
            assert!(e.contains(want), "{q}\n  -> {e}");
        }
    }
}
