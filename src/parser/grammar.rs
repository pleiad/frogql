use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::dm::{validate_insert_pattern, DmOp, DmStatement, RemoveItem, SetItem};
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::path_prefix::{PathMode, PathPrefix, PathSearch};
use crate::syntax::query::{
    Aggregator, GeneralSetKind, MatchStatement, NullsOrder, Query, ReturnItem, SetQuantifier,
    SortDir, SortKey, SortSpec,
};
use crate::syntax::statement::{IndexKindStmt, Statement, TypeElement};
use crate::typing::descriptor_type::DescriptorType;
use crate::typing::label_type::LabelType;
use crate::typing::property_type::PropertyType;
use crate::typing::simple_type::SimpleType;
use crate::typing::variable_type::VariableType;

use super::lexer::{Lexer, Token};

/// Parse a GQL path pattern string into a PathPattern (backwards compatible).
pub fn parse(input: &str) -> Result<PathPattern, String> {
    let q = parse_query(input)?;
    Ok(q.collapsed_pattern())
}

/// Parse a full GQL query: optional MATCH, path pattern, optional WHERE, optional RETURN.
pub fn parse_query(input: &str) -> Result<Query, String> {
    let tokens = Lexer::tokenize(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let result = p.full_query()?;
    p.eat(&Token::Semicolon);
    if !p.at_eof() {
        return Err(format!(
            "unexpected token {:?} at position {}",
            p.peek(),
            p.pos
        ));
    }
    Ok(result)
}

/// Parse a single top-level statement: a query or a catalog DDL command.
/// Trailing `;` is optional; the entry point that handles multi-statement
/// input belongs in callers (REPL, Python bindings).
pub fn parse_statement(input: &str) -> Result<Statement, String> {
    let tokens = Lexer::tokenize(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let stmt = p.statement()?;
    p.eat(&Token::Semicolon);
    if !p.at_eof() {
        return Err(format!(
            "unexpected token {:?} at position {}",
            p.peek(),
            p.pos
        ));
    }
    Ok(stmt)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset)
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.pos += 1;
        t
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(format!("expected {expected:?}, got {:?}", self.peek()))
        }
    }

    fn check(&self, tok: &Token) -> bool {
        self.peek() == tok
    }

    /// Look ahead for `IS NULL` or `IS NOT NULL`. On match, consume the
    /// tokens and return the original `left` operand together with the
    /// negation flag. Otherwise the parser state is untouched.
    fn try_is_null(&mut self, left: &Expr) -> Option<(Expr, bool)> {
        if !matches!(self.peek(), Token::Typed) {
            return None;
        }
        let next = self.peek_at(1)?;
        match next {
            Token::Null => {
                self.advance(); // is
                self.advance(); // null
                Some((left.clone(), false))
            }
            Token::Not => {
                if matches!(self.peek_at(2), Some(Token::Null)) {
                    self.advance(); // is
                    self.advance(); // not
                    self.advance(); // null
                    Some((left.clone(), true))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn eat(&mut self, tok: &Token) -> bool {
        if self.check(tok) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check_name_keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Token::Name(s) if s.eq_ignore_ascii_case(kw))
    }

    fn peek_name_keyword(&self, offset: usize, kw: &str) -> bool {
        matches!(self.peek_at(offset), Some(Token::Name(s)) if s.eq_ignore_ascii_case(kw))
    }

    fn eat_name_keyword(&mut self, kw: &str) -> bool {
        if self.check_name_keyword(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_name_keyword(&mut self, kw: &str) -> Result<(), String> {
        if self.eat_name_keyword(kw) {
            Ok(())
        } else {
            Err(format!("expected {kw}, got {:?}", self.peek()))
        }
    }

    // ISO §14.3-14.4 + §14.9 + §14.11 + §16.17.
    //
    //   full_query   = match_clause+ legacy_group_by?
    //                  ("RETURN" ("DISTINCT")? return_list group_by?)?
    //                  ("ORDER BY" sort_spec (, sort_spec)*)?
    //                  ("LIMIT" integer)?
    //   sort_spec    = expr ("ASC"|"DESC"|"ASCENDING"|"DESCENDING")?
    //                  ("NULLS FIRST"|"NULLS LAST")?
    //   match_clause = ("OPTIONAL" "MATCH" | "MATCH"?) query ("WHERE" expr)?
    //
    // GROUP BY: ISO §14.11 places it INSIDE `<return statement body>`,
    // after the item list. Pre-72a6449e gqlite put it between WHERE and
    // RETURN; both positions are accepted (specifying both is an error).
    fn full_query(&mut self) -> Result<Query, String> {
        let first_optional = self.eat(&Token::Optional);
        if first_optional {
            self.expect(&Token::Match)?;
        } else {
            self.eat(&Token::Match);
        }
        let first_stmt = self.match_statement(first_optional)?;
        let matches = self.continue_match_chain(vec![first_stmt])?;
        self.finish_query_after_matches(matches)
    }

    /// Continue a partially-parsed `Vec<MatchStatement>` by consuming any
    /// trailing `(OPTIONAL)? MATCH ...` clauses. Stops at the first token
    /// that is neither MATCH nor OPTIONAL.
    fn continue_match_chain(
        &mut self,
        mut matches: Vec<MatchStatement>,
    ) -> Result<Vec<MatchStatement>, String> {
        loop {
            if self.eat(&Token::Optional) {
                self.expect(&Token::Match)?;
                matches.push(self.match_statement(true)?);
            } else if self.eat(&Token::Match) {
                matches.push(self.match_statement(false)?);
            } else {
                break;
            }
        }
        Ok(matches)
    }

    /// Parse an explicit MATCH chain that starts with `MATCH` or
    /// `OPTIONAL MATCH`. Used by the DML dispatcher; differs from
    /// `full_query` in that the leading MATCH keyword is required.
    fn parse_match_chain_explicit(&mut self) -> Result<Vec<MatchStatement>, String> {
        let first_optional = self.eat(&Token::Optional);
        self.expect(&Token::Match)?;
        let first_stmt = self.match_statement(first_optional)?;
        self.continue_match_chain(vec![first_stmt])
    }

    /// Finish parsing a `<linear query statement>` once the MATCH chain
    /// is in hand: GROUP BY (legacy or canonical), RETURN, ORDER BY, LIMIT.
    fn finish_query_after_matches(
        &mut self,
        matches: Vec<MatchStatement>,
    ) -> Result<Query, String> {
        // Legacy position: GROUP BY between the match chain and RETURN.
        // Pre-ISO form kept for back-compat (commit 72a6449e). The
        // canonical post-items position below is ISO §14.11.
        let legacy_group_by = self.parse_group_by_clause()?;

        let (returns, distinct, post_return_group_by) = if self.eat(&Token::Return) {
            let distinct = self.eat(&Token::Distinct);
            let items = self.return_list()?;
            // ISO §14.11 <return statement body>: GROUP BY clause
            // immediately follows the <return item list>.
            let gb = self.parse_group_by_clause()?;
            (Some(items), distinct, gb)
        } else {
            (None, false, None)
        };

        let group_by = match (legacy_group_by, post_return_group_by) {
            (Some(_), Some(_)) => {
                return Err(
                    "GROUP BY appears both before RETURN and inside the RETURN body — \
                     specify it only once (the canonical position is after the return items per ISO §14.11)"
                        .into(),
                );
            }
            (Some(gb), None) | (None, Some(gb)) => Some(gb),
            (None, None) => None,
        };

        let order_by = self.parse_optional_order_by(returns.as_deref())?;
        let limit = self.parse_optional_limit()?;

        Ok(Query {
            matches,
            group_by,
            returns,
            distinct,
            order_by,
            limit,
        })
    }

    fn parse_group_by_clause(&mut self) -> Result<Option<Vec<Expr>>, String> {
        if !self.eat(&Token::GroupBy) {
            return Ok(None);
        }
        let mut exprs = vec![self.expr()?];
        while self.eat(&Token::Comma) {
            exprs.push(self.expr()?);
        }
        Ok(Some(exprs))
    }

    /// Optional trailing `LIMIT N`. Returns `None` if absent.
    /// Negative or out-of-u32-range integers are parse errors.
    fn parse_optional_limit(&mut self) -> Result<Option<u32>, String> {
        if !self.eat(&Token::Limit) {
            return Ok(None);
        }
        let n = self
            .expect_number()
            .map_err(|e| format!("LIMIT requires a non-negative integer: {e}"))?;
        if n < 0 {
            return Err(format!("LIMIT requires a non-negative integer, got {n}"));
        }
        u32::try_from(n)
            .map(Some)
            .map_err(|_| format!("LIMIT {n} exceeds the supported u32 range"))
    }

    /// `returns` is the already-parsed `<return item list>` for §16.17
    /// SR 5c alias resolution; pass `None` when there is no RETURN.
    fn parse_optional_order_by(
        &mut self,
        returns: Option<&[ReturnItem]>,
    ) -> Result<Option<Vec<SortSpec>>, String> {
        if !self.eat(&Token::OrderBy) {
            return Ok(None);
        }
        let mut specs = vec![self.sort_spec(returns)?];
        while self.eat(&Token::Comma) {
            specs.push(self.sort_spec(returns)?);
        }
        Ok(Some(specs))
    }

    fn sort_spec(&mut self, returns: Option<&[ReturnItem]>) -> Result<SortSpec, String> {
        let key = self.parse_sort_key(returns)?;
        let dir = match self.peek() {
            Token::Asc | Token::Ascending => {
                self.advance();
                SortDir::Asc
            }
            Token::Desc | Token::Descending => {
                self.advance();
                SortDir::Desc
            }
            _ => SortDir::Asc,
        };
        let nulls = match self.peek() {
            Token::NullsFirst => {
                self.advance();
                Some(NullsOrder::First)
            }
            Token::NullsLast => {
                self.advance();
                Some(NullsOrder::Last)
            }
            _ => None,
        };
        Ok(SortSpec { key, dir, nulls })
    }

    /// Resolve a sort key (ISO §16.17 SR 1 + 5c). Order: bare-name
    /// alias → direct aggregate (must match a `ReturnItem::Aggregate`)
    /// → free `Expr` (aggregate queries get a structural-match pass
    /// against non-aggregate RETURN items so post-projection sort can
    /// look them up).
    fn parse_sort_key(&mut self, returns: Option<&[ReturnItem]>) -> Result<SortKey, String> {
        if let (Some(items), Token::Name(name)) = (returns, self.peek().clone()) {
            let alias_idx = items.iter().position(|it| it.alias() == Some(&name));
            if !matches!(self.peek_at(1), Some(Token::Dot)) {
                if let Some(idx) = alias_idx {
                    self.advance();
                    return Ok(SortKey::Column(idx));
                }
            } else if let Some(col) = alias_idx {
                // `<alias>.<field>[.<field>...]` — sort by a field path
                // into a record-valued projected column (e.g. a
                // `VALUE { ... } AS latestLike` then `latestLike.x`).
                self.advance(); // alias name
                let mut path = Vec::new();
                while self.eat(&Token::Dot) {
                    match self.advance() {
                        Token::Name(field) => path.push(field),
                        t => return Err(format!("expected field name after '.', got {t:?}")),
                    }
                }
                return Ok(SortKey::ColumnField { col, path });
            }
        }

        if returns.is_some() && self.peek_aggregate_kind().is_some() {
            let agg = self.aggregate_function()?;
            if let Some(items) = returns {
                for (idx, item) in items.iter().enumerate() {
                    if let ReturnItem::Aggregate { agg: ret_agg, .. } = item {
                        if *ret_agg == agg {
                            return Ok(SortKey::Column(idx));
                        }
                    }
                }
            }
            return Err(format!(
                "ORDER BY {agg} is not in the RETURN list — direct aggregates in \
                 sort keys must also appear as a RETURN item (alias optional)."
            ));
        }

        let expr = self.expr()?;
        if let Some(items) = returns {
            if let Some((col, ty)) = self.casted_alias_sort_key(&expr, items) {
                return Ok(SortKey::ColumnCast { col, ty });
            }
            if items.iter().any(|it| it.is_aggregate()) {
                for (idx, item) in items.iter().enumerate() {
                    if let ReturnItem::Expr { expr: ret_expr, .. } = item {
                        if *ret_expr == expr {
                            return Ok(SortKey::Column(idx));
                        }
                    }
                }
            }
        }
        Ok(SortKey::Expr(expr))
    }

    fn casted_alias_sort_key(
        &self,
        expr: &Expr,
        returns: &[ReturnItem],
    ) -> Option<(usize, SimpleType)> {
        let Expr::Call { name, args } = expr else {
            return None;
        };
        if name != "CAST" || args.len() != 2 {
            return None;
        }
        let Expr::Var(alias) = &args[0] else {
            return None;
        };
        let Expr::Type(ty) = &args[1] else {
            return None;
        };
        returns
            .iter()
            .position(|it| it.alias() == Some(alias.as_str()))
            .map(|col| (col, ty.clone()))
    }

    /// One match clause: pattern + optional WHERE wrapped in `Filter`.
    /// Per-clause WHERE is scoped — `MATCH (x) WHERE _ MATCH (y) WHERE _`
    /// produces two independent `Filter`-wrapped patterns, not a single
    /// AND-ed predicate.
    fn match_clause_body(&mut self) -> Result<PathPattern, String> {
        let mut pattern = self.query()?;
        if self.eat(&Token::Where) {
            let expr = self.expr()?;
            pattern = PathPattern::Filter(Box::new(pattern), expr);
        }
        Ok(pattern)
    }

    /// MATCH / OPTIONAL MATCH wrapper; prefixes are parsed by `query()`.
    fn match_statement(&mut self, optional: bool) -> Result<MatchStatement, String> {
        let pattern = self.match_clause_body()?;
        Ok(if optional {
            MatchStatement::Optional { pattern }
        } else {
            MatchStatement::Simple { pattern }
        })
    }

    /// ISO §16.6 prefix at the start of one comma operand.
    fn parse_path_prefix(&mut self) -> Result<Option<PathPrefix>, String> {
        let prefix = if self.eat(&Token::All) {
            // ALL [SHORTEST] [<mode>] [PATH|PATHS]
            if self.eat_keyword("SHORTEST") {
                let mode = self.eat_path_mode().unwrap_or(PathMode::Walk);
                self.eat_path_or_paths();
                PathPrefix {
                    mode,
                    search: PathSearch::ShortestGroups { count: 1 },
                }
            } else {
                let mode = self.eat_path_mode().unwrap_or(PathMode::Walk);
                self.eat_path_or_paths();
                PathPrefix {
                    mode,
                    search: PathSearch::All,
                }
            }
        } else if self.eat_any_path_prefix() {
            // ANY: ANY [SHORTEST | <number>] [<mode>] [PATH|PATHS]
            if self.eat_keyword("SHORTEST") {
                let mode = self.eat_path_mode().unwrap_or(PathMode::Walk);
                self.eat_path_or_paths();
                PathPrefix {
                    mode,
                    search: PathSearch::ShortestPaths { count: 1 },
                }
            } else {
                let count = self.eat_path_count()?.unwrap_or(1);
                let mode = self.eat_path_mode().unwrap_or(PathMode::Walk);
                self.eat_path_or_paths();
                PathPrefix {
                    mode,
                    search: PathSearch::Any { count },
                }
            }
        } else if self.eat_keyword("SHORTEST") {
            // SHORTEST <number> [<mode>] [PATH|PATHS]                  (counted shortest path)
            // SHORTEST [<number>] [<mode>] [PATH|PATHS] {GROUP|GROUPS} (counted shortest group)
            let count = self.eat_path_count()?;
            let mode = self.eat_path_mode().unwrap_or(PathMode::Walk);
            self.eat_path_or_paths();
            if self.eat_keyword("GROUP") || self.eat_keyword("GROUPS") {
                PathPrefix {
                    mode,
                    search: PathSearch::ShortestGroups {
                        count: count.unwrap_or(1),
                    },
                }
            } else {
                let count = count.ok_or_else(|| {
                    "SHORTEST requires a positive path count (e.g. `SHORTEST 1`) \
                     or the GROUP / GROUPS keyword"
                        .to_string()
                })?;
                PathPrefix {
                    mode,
                    search: PathSearch::ShortestPaths { count },
                }
            }
        } else if let Some(mode) = self.eat_path_mode() {
            self.eat_path_or_paths();
            PathPrefix {
                mode,
                search: PathSearch::All,
            }
        } else {
            return Ok(None);
        };

        if prefix.is_trivial() {
            Ok(None)
        } else {
            Ok(Some(prefix))
        }
    }

    /// Consume the current token iff it is a `Name` equal (case-insensitive)
    /// to `kw`. Used for the soft keywords of the path prefix grammar.
    fn eat_keyword(&mut self, kw: &str) -> bool {
        let matched = matches!(self.peek(), Token::Name(s) if s.eq_ignore_ascii_case(kw));
        if matched {
            self.advance();
        }
        matched
    }

    /// Accept lowercase `any` as a soft keyword only in prefix position.
    fn eat_any_path_prefix(&mut self) -> bool {
        if self.eat(&Token::Any) {
            return true;
        }
        self.eat_keyword("ANY")
    }

    /// ISO §16.6 `<path mode>` keyword (WALK/TRAIL/SIMPLE/ACYCLIC), consumed
    /// when present.
    fn eat_path_mode(&mut self) -> Option<PathMode> {
        let mode = match self.peek() {
            Token::Name(s) => match s.to_ascii_uppercase().as_str() {
                "WALK" => PathMode::Walk,
                "TRAIL" => PathMode::Trail,
                "SIMPLE" => PathMode::Simple,
                "ACYCLIC" => PathMode::Acyclic,
                _ => return None,
            },
            _ => return None,
        };
        self.advance();
        Some(mode)
    }

    /// Optional `<path or paths>` (the noise words PATH / PATHS), discarded.
    fn eat_path_or_paths(&mut self) {
        let _ = self.eat_keyword("PATH") || self.eat_keyword("PATHS");
    }

    /// ISO §16.6 `<number of paths>` / `<number of groups>`. When a literal
    /// is present it must be a positive integer (SR 2b).
    fn eat_path_count(&mut self) -> Result<Option<usize>, String> {
        if let Token::Number(n) = *self.peek() {
            self.advance();
            if n <= 0 {
                return Err(format!(
                    "path search count must be a positive integer, got {n}"
                ));
            }
            Ok(Some(n as usize))
        } else {
            Ok(None)
        }
    }

    // return_list = return_item ("," return_item)*
    fn return_list(&mut self) -> Result<Vec<ReturnItem>, String> {
        let mut items = vec![self.return_item()?];
        while self.eat(&Token::Comma) {
            items.push(self.return_item()?);
        }
        Ok(items)
    }

    // return_item = expr alias?
    //
    // Aggregates are parsed as part of the expression grammar (see
    // `primary_expr`), so `COUNT(x) + COUNT(y)` lands here as a
    // `Binop` over two `Expr::Agg` operands. A *bare* top-level
    // aggregate is re-folded into `ReturnItem::Aggregate` so the
    // existing aggregate-projection and ORDER BY-matching paths stay
    // unchanged.
    fn return_item(&mut self) -> Result<ReturnItem, String> {
        let expr = self.return_expr()?;
        let alias = self.maybe_alias();
        if let Expr::Agg(agg) = expr {
            return Ok(ReturnItem::Aggregate { agg: *agg, alias });
        }
        Ok(ReturnItem::Expr { expr, alias })
    }

    fn peek_aggregate_kind(&self) -> Option<()> {
        let lparen_next = matches!(self.peek_at(1), Some(Token::LParen));
        if !lparen_next {
            return None;
        }
        match self.peek() {
            Token::Count
            | Token::Sum
            | Token::Avg
            | Token::Min
            | Token::Max
            | Token::CollectList => Some(()),
            _ => None,
        }
    }

    fn aggregate_function(&mut self) -> Result<Aggregator, String> {
        let kind_tok = self.advance();
        self.expect(&Token::LParen)?;

        // COUNT(*) is the only ISO §20.9 form that takes `*`.
        if matches!(kind_tok, Token::Count) && self.eat(&Token::Star) {
            self.expect(&Token::RParen)?;
            return Ok(Aggregator::CountStar);
        }

        // ISO §20.9 syntax rule 2: ALL is implicit when omitted.
        let quantifier = if self.eat(&Token::Distinct) {
            SetQuantifier::Distinct
        } else {
            // Consume an optional ALL token if present; the result is the
            // same SetQuantifier::All either way.
            self.eat(&Token::All);
            SetQuantifier::All
        };
        let expr = self.expr()?;
        self.expect(&Token::RParen)?;

        let kind = match kind_tok {
            Token::Count => GeneralSetKind::Count,
            Token::Sum => GeneralSetKind::Sum,
            Token::Avg => GeneralSetKind::Avg,
            Token::Min => GeneralSetKind::Min,
            Token::Max => GeneralSetKind::Max,
            Token::CollectList => GeneralSetKind::CollectList,
            _ => unreachable!("peek_aggregate_kind already filtered the keyword"),
        };
        Ok(Aggregator::GeneralSet {
            kind,
            quantifier,
            expr,
        })
    }

    /// AS is also a type-cast operator in exprs; here it's unambiguously
    /// an alias when followed by a Name.
    fn maybe_alias(&mut self) -> Option<String> {
        if self.check(&Token::As) {
            let saved = self.pos;
            self.advance(); // consume AS
            if let Token::Name(n) = self.peek().clone() {
                self.advance();
                return Some(n);
            }
            self.pos = saved;
        }
        None
    }

    // Like expr but excludes AS from comparison operators (reserved for alias).
    fn return_expr(&mut self) -> Result<Expr, String> {
        self.return_logical_op()
    }

    fn return_logical_op(&mut self) -> Result<Expr, String> {
        let mut left = self.return_comparison()?;
        loop {
            let op = match self.peek() {
                Token::And => BinOp::And,
                Token::Or => BinOp::Or,
                _ => break,
            };
            self.advance();
            let right = self.return_comparison()?;
            left = Expr::Binop {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // Like comparison() but without AS (so it's available for alias).
    fn return_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        loop {
            if let Some((operand, negated)) = self.try_is_null(&left) {
                left = Expr::IsNull {
                    operand: Box::new(operand),
                    negated,
                };
                continue;
            }
            let op = match self.peek() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::Le => BinOp::Le,
                Token::Ge => BinOp::Ge,
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Typed => BinOp::Is,
                // Implicit type predicate: a type-head token after a term.
                Token::Int
                | Token::Float
                | Token::Bool
                | Token::Str
                | Token::Any
                | Token::Star
                | Token::LBracket
                | Token::LBrace => {
                    let right = self.term()?;
                    left = Expr::Binop {
                        op: BinOp::Is,
                        left: Box::new(left),
                        right: Box::new(right),
                    };
                    continue;
                }
                // No Token::As here — reserved for alias
                _ => break,
            };
            self.advance();
            let right = self.term()?;
            left = Expr::Binop {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // ===== Queries (comma-join level) =====

    // Prefixes are per comma operand, not per MATCH clause.
    fn query(&mut self) -> Result<PathPattern, String> {
        let mut left = self.path_pattern_operand()?;
        while self.eat(&Token::Comma) {
            let right = self.path_pattern_operand()?;
            left = PathPattern::Join(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// Parse one comma operand, wrapping meaningful prefixes in `Selected`
    /// and an optional `<path variable declaration>` in `Named`.
    fn path_pattern_operand(&mut self) -> Result<PathPattern, String> {
        // ISO `<path variable declaration> ::= <binding variable> =`.
        // A bare `Name =` at the start of an operand can only be a path
        // variable binding: a comparison `=` never begins an operand, and
        // the prefix/pattern grammar that follows never starts with `Name =`.
        let path_var = if matches!(self.peek(), Token::Name(_))
            && matches!(self.peek_at(1), Some(Token::Eq))
        {
            let name = match self.advance() {
                Token::Name(n) => n,
                _ => unreachable!("guarded by peek above"),
            };
            self.advance(); // consume '='
            Some(name)
        } else {
            None
        };

        let prefix = self.parse_path_prefix()?;
        let pattern = self.path_pattern()?;
        let pattern = match prefix {
            Some(prefix) => PathPattern::Selected {
                prefix,
                pattern: Box::new(pattern),
            },
            None => pattern,
        };
        Ok(match path_var {
            Some(var) => PathPattern::Named {
                var,
                pattern: Box::new(pattern),
            },
            None => pattern,
        })
    }

    // ===== Path patterns =====

    // path_pattern = path_term ("|" path_term)*
    fn path_pattern(&mut self) -> Result<PathPattern, String> {
        let mut left = self.path_term()?;
        while self.eat(&Token::Pipe) {
            let right = self.path_term()?;
            left = PathPattern::Union(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // path_term = path_factor+
    fn path_term(&mut self) -> Result<PathPattern, String> {
        let mut left = self.path_factor()?;
        while self.is_path_factor_start() {
            let right = self.path_factor()?;
            left = PathPattern::Concat(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn is_path_factor_start(&self) -> bool {
        matches!(
            self.peek(),
            Token::LParen
                | Token::DashLB
                | Token::LtDashLB
                | Token::TildeLB
                | Token::RightArrow
                | Token::LeftArrow
                | Token::Tilde
                | Token::Minus
        )
    }

    // path_factor = path_primary quantifier?
    fn path_factor(&mut self) -> Result<PathPattern, String> {
        let primary = self.path_primary()?;

        // Check for quantifier: {n,m}, *, +, ?
        if self.eat(&Token::LBrace) {
            let (lb, ub) = self.quantifier_body()?;
            self.expect(&Token::RBrace)?;
            return Ok(PathPattern::Repeat {
                pattern: Box::new(primary),
                lb,
                ub,
            });
        }
        if self.eat(&Token::Star) {
            return Ok(PathPattern::Repeat {
                pattern: Box::new(primary),
                lb: 0,
                ub: None,
            });
        }
        if self.eat(&Token::Plus) {
            return Ok(PathPattern::Repeat {
                pattern: Box::new(primary),
                lb: 1,
                ub: None,
            });
        }
        if self.eat(&Token::Question) {
            return Ok(PathPattern::Questioned(Box::new(primary)));
        }

        Ok(primary)
    }

    // {lb, ub} | {n}
    fn quantifier_body(&mut self) -> Result<(usize, Option<usize>), String> {
        let first = self.expect_number()? as usize;
        if self.eat(&Token::Comma) {
            if let Token::Number(n) = self.peek().clone() {
                self.advance();
                Ok((first, Some(n as usize)))
            } else {
                // {n,} — no upper bound
                Ok((first, None))
            }
        } else {
            // {n} — fixed repetition
            Ok((first, Some(first)))
        }
    }

    fn expect_number(&mut self) -> Result<i64, String> {
        match self.advance() {
            Token::Number(n) => Ok(n),
            t => Err(format!("expected number, got {t:?}")),
        }
    }

    // path_primary = node_pattern | edge_pattern | filter "(" path_pattern ")" | "(" path_pattern ")"
    fn path_primary(&mut self) -> Result<PathPattern, String> {
        match self.peek().clone() {
            Token::LParen => self.paren_pattern(),
            Token::DashLB => self.full_edge_right_or_any(),
            Token::LtDashLB => self.full_edge_left(),
            Token::TildeLB => self.full_edge_undirected(),
            Token::RightArrow => {
                self.advance();
                Ok(PathPattern::EdgeRight(Some(Descriptor::type_only(
                    DescriptorType::star(),
                ))))
            }
            Token::LeftArrow => {
                self.advance();
                Ok(PathPattern::EdgeLeft(Some(Descriptor::type_only(
                    DescriptorType::star(),
                ))))
            }
            Token::Tilde => {
                self.advance();
                Ok(PathPattern::EdgeUndirected(Some(Descriptor::type_only(
                    DescriptorType::star(),
                ))))
            }
            Token::Minus => {
                self.advance();
                Ok(PathPattern::EdgeAnyDirection(Some(Descriptor::type_only(
                    DescriptorType::star(),
                ))))
            }
            _ => Err(format!("expected path pattern, got {:?}", self.peek())),
        }
    }

    // "(" ... ")" — could be node_pattern, filter_pattern, or grouped path_pattern
    fn paren_pattern(&mut self) -> Result<PathPattern, String> {
        self.expect(&Token::LParen)?;

        // "()" — empty node
        if self.check(&Token::RParen) {
            self.advance();
            return Ok(PathPattern::Node(Some(Descriptor::type_only(
                DescriptorType::star(),
            ))));
        }

        // Try to parse as element_pattern_filler (variable? : type_schema? WHERE expr?)
        // This covers node patterns like (x), (x: Label), (:Label {prop: int}), (x WHERE ...)
        // But also filter patterns like (path_pattern WHERE expr)

        // Peek ahead: if we see a path pattern start after "(", it might be a grouped pattern
        // or a filter. The key insight: a Name followed by certain tokens determines what it is.
        let saved = self.pos;

        // Try parsing as element_pattern_filler first
        if let Ok((desc, where_expr)) = self.try_element_pattern_filler() {
            if self.eat(&Token::RParen) {
                let node = PathPattern::Node(Some(desc));
                return match where_expr {
                    Some(e) => Ok(PathPattern::Filter(Box::new(node), e)),
                    None => Ok(node),
                };
            }
        }

        // Backtrack and try as grouped path_pattern or filter_pattern
        self.pos = saved;
        // Skip the LParen we already consumed — wait, we need to re-consume it
        // Actually we already consumed it before. The saved pos is after LParen.

        let inner = self.path_pattern()?;
        if self.eat(&Token::Where) {
            let expr = self.expr()?;
            self.expect(&Token::RParen)?;
            Ok(PathPattern::Filter(Box::new(inner), expr))
        } else {
            self.expect(&Token::RParen)?;
            Ok(inner)
        }
    }

    // Try to parse element_pattern_filler:
    //   variable? (":" type_schema | "{" record_type)? (WHERE expr)?
    // Per ISO/IEC 39075:2024 §16, `elementPropertySpecification` ({...}) is a
    // sibling of `isLabelExpression`, so the colon is optional in front of
    // the record. `({k: v})` and `(x {k: v})` are both valid node patterns.
    fn try_element_pattern_filler(&mut self) -> Result<(Descriptor, Option<Expr>), String> {
        let mut var: Option<String> = None;
        let mut dtype: Option<DescriptorType> = None;
        let mut value_filters: Vec<(String, Expr)> = Vec::new();
        let mut where_expr: Option<Expr> = None;

        // Optional variable
        if let Token::Name(name) = self.peek().clone() {
            let saved = self.pos;
            self.advance();
            match self.peek() {
                Token::Colon | Token::Where | Token::RParen | Token::LBrace => {
                    var = Some(name);
                }
                _ => {
                    self.pos = saved;
                    return Err("not an element pattern filler".into());
                }
            }
        }

        // Optional ": type_schema" or bare "{ record_type }". The LBrace-first
        // arm of `type_schema` defaults the label to Star, which matches the
        // ISO semantics for an absent isLabelExpression.
        if self.eat(&Token::Colon) || matches!(self.peek(), Token::LBrace) {
            let (dt, filters) = self.type_schema()?;
            dtype = Some(dt);
            value_filters = filters;
        }

        // Optional WHERE expr
        if self.eat(&Token::Where) {
            where_expr = Some(self.expr()?);
        }

        let dt = dtype.unwrap_or_else(DescriptorType::star);
        Ok((Descriptor::with_filters(var, dt, value_filters), where_expr))
    }

    // -[ filler ]-> or -[ filler ]-
    fn full_edge_right_or_any(&mut self) -> Result<PathPattern, String> {
        self.expect(&Token::DashLB)?;

        // -[]-> or -[]-
        if self.check(&Token::RBDashGt) {
            self.advance();
            return Ok(PathPattern::EdgeRight(Some(Descriptor::type_only(
                DescriptorType::star(),
            ))));
        }
        if self.check(&Token::RBDash) {
            self.advance();
            return Ok(PathPattern::EdgeAnyDirection(Some(Descriptor::type_only(
                DescriptorType::star(),
            ))));
        }

        let (desc, where_expr) = self.edge_filler()?;

        if self.eat(&Token::RBDashGt) {
            let edge = PathPattern::EdgeRight(Some(desc));
            Ok(match where_expr {
                Some(e) => PathPattern::Filter(Box::new(edge), e),
                None => edge,
            })
        } else if self.eat(&Token::RBDash) {
            let edge = PathPattern::EdgeAnyDirection(Some(desc));
            Ok(match where_expr {
                Some(e) => PathPattern::Filter(Box::new(edge), e),
                None => edge,
            })
        } else {
            Err(format!("expected ]-> or ]-, got {:?}", self.peek()))
        }
    }

    // <-[ filler ]-
    fn full_edge_left(&mut self) -> Result<PathPattern, String> {
        self.expect(&Token::LtDashLB)?;

        if self.check(&Token::RBDash) {
            self.advance();
            return Ok(PathPattern::EdgeLeft(Some(Descriptor::type_only(
                DescriptorType::star(),
            ))));
        }

        let (desc, where_expr) = self.edge_filler()?;
        self.expect(&Token::RBDash)?;

        let edge = PathPattern::EdgeLeft(Some(desc));
        Ok(match where_expr {
            Some(e) => PathPattern::Filter(Box::new(edge), e),
            None => edge,
        })
    }

    // ~[ filler ]~
    fn full_edge_undirected(&mut self) -> Result<PathPattern, String> {
        self.expect(&Token::TildeLB)?;

        if self.check(&Token::RBTilde) {
            self.advance();
            return Ok(PathPattern::EdgeUndirected(Some(Descriptor::type_only(
                DescriptorType::star(),
            ))));
        }

        let (desc, where_expr) = self.edge_filler()?;
        self.expect(&Token::RBTilde)?;

        let edge = PathPattern::EdgeUndirected(Some(desc));
        Ok(match where_expr {
            Some(e) => PathPattern::Filter(Box::new(edge), e),
            None => edge,
        })
    }

    // Parse the inside of an edge bracket: variable? (":" type_schema)? (WHERE expr)?
    fn edge_filler(&mut self) -> Result<(Descriptor, Option<Expr>), String> {
        let mut var: Option<String> = None;
        let mut dtype: Option<DescriptorType> = None;
        let mut value_filters: Vec<(String, Expr)> = Vec::new();
        let mut where_expr: Option<Expr> = None;

        if let Token::Name(name) = self.peek().clone() {
            self.advance();
            var = Some(name);
        }

        // Optional ": type_schema" or bare "{ record_type }" — same ISO §16
        // shape as node fillers. `-[{since: 2020}]->` and
        // `-[e {since: 2020}]->` are valid edge patterns.
        if self.eat(&Token::Colon) || matches!(self.peek(), Token::LBrace) {
            let (dt, filters) = self.type_schema()?;
            dtype = Some(dt);
            value_filters = filters;
        }

        if self.eat(&Token::Where) {
            where_expr = Some(self.expr()?);
        }

        let dt = dtype.unwrap_or_else(DescriptorType::star);
        Ok((Descriptor::with_filters(var, dt, value_filters), where_expr))
    }

    // ===== Type schema =====

    // type_schema = label_pattern record_type | label_pattern | record_type
    fn type_schema(&mut self) -> Result<(DescriptorType, Vec<(String, Expr)>), String> {
        // Could start with label or record type
        match self.peek() {
            Token::LBrace => {
                // Record type only (no label)
                let (rt, filters) = self.record_type()?;
                Ok((DescriptorType::new(LabelType::Star, rt), filters))
            }
            _ => {
                // Label pattern, optionally followed by record type
                let label = self.label_pattern()?;
                if matches!(self.peek(), Token::LBrace) {
                    let (rt, filters) = self.record_type()?;
                    Ok((DescriptorType::new(label, rt), filters))
                } else {
                    Ok((
                        DescriptorType::new(label, PropertyType::open_empty()),
                        Vec::new(),
                    ))
                }
            }
        }
    }

    // label_pattern = label_term ("|" label_term)*
    fn label_pattern(&mut self) -> Result<LabelType, String> {
        let mut left = self.label_term()?;
        while self.eat(&Token::Pipe) {
            let right = self.label_term()?;
            left = LabelType::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // label_term = label_factor ("&" label_factor)*
    fn label_term(&mut self) -> Result<LabelType, String> {
        let mut left = self.label_factor()?;
        while self.eat(&Token::Amp) {
            let right = self.label_factor()?;
            left = LabelType::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // label_factor = "!" label_primary | label_primary
    fn label_factor(&mut self) -> Result<LabelType, String> {
        if self.eat(&Token::Bang) {
            let inner = self.label_primary()?;
            Ok(LabelType::Neg(Box::new(inner)))
        } else {
            self.label_primary()
        }
    }

    // label_primary = NAME | "*" | "(" label_pattern ")"
    fn label_primary(&mut self) -> Result<LabelType, String> {
        match self.peek().clone() {
            Token::Name(name) => {
                self.advance();
                Ok(LabelType::Label(name))
            }
            // `ANY` is an alias for the `*` label wildcard. It lexes to a
            // distinct token (so the §16.6 path-prefix grammar can tell
            // `ANY <pattern>` from a `*` type wildcard), but in label
            // position it means the same "any label" as `*`.
            Token::Star | Token::Any => {
                self.advance();
                Ok(LabelType::Star)
            }
            Token::LParen => {
                self.advance();
                let lp = self.label_pattern()?;
                self.expect(&Token::RParen)?;
                Ok(lp)
            }
            _ => Err(format!("expected label, got {:?}", self.peek())),
        }
    }

    // record_type = open_record_type ({...}) | closed_record_type ({{...}})
    // Closed records use two adjacent single braces; the lexer no longer coalesces
    // `{{` / `}}` into special tokens so nested records like `{a is {b is int}}`
    // parse correctly.
    fn record_type(&mut self) -> Result<(PropertyType, Vec<(String, Expr)>), String> {
        self.expect(&Token::LBrace)?;
        let is_closed = self.eat(&Token::LBrace);
        let mut pt = if is_closed {
            PropertyType::closed_empty()
        } else {
            PropertyType::open_empty()
        };
        let mut filters = Vec::new();
        if self.eat(&Token::RBrace) {
            if is_closed {
                self.expect(&Token::RBrace)?;
            }
            return Ok((pt, filters));
        }
        self.parse_record_elements(&mut pt, &mut filters)?;
        self.expect(&Token::RBrace)?;
        if is_closed {
            self.expect(&Token::RBrace)?;
        }
        Ok((pt, filters))
    }

    fn parse_record_elements(
        &mut self,
        pt: &mut PropertyType,
        filters: &mut Vec<(String, Expr)>,
    ) -> Result<(), String> {
        self.record_element(pt, filters)?;
        while self.eat(&Token::Comma) {
            self.record_element(pt, filters)?;
        }
        Ok(())
    }

    /// Parse one record element. ISO GQL distinguishes types from values via
    /// the `<typed>` element (optional `::` or `TYPED`) versus `:` for values:
    ///   `name T`         — type ascription, implicit (no separator)
    ///   `name :: T`      — type ascription, explicit
    ///   `name TYPED T`   — type ascription, explicit keyword
    ///   `name : e`       — value-equality filter (elaborated to `name = e`)
    fn record_element(
        &mut self,
        pt: &mut PropertyType,
        filters: &mut Vec<(String, Expr)>,
    ) -> Result<(), String> {
        let name = match self.advance() {
            Token::Name(n) => n,
            t => return Err(format!("expected attribute name, got {t:?}")),
        };
        match self.peek() {
            Token::DoubleColon | Token::Typed => {
                self.advance();
                let ty = self.simple_type()?;
                pt.extend(name, ty);
            }
            // Implicit type ascription: type-head token directly after the name.
            Token::Int
            | Token::Float
            | Token::Bool
            | Token::Str
            | Token::Any
            | Token::Star
            | Token::LBracket
            | Token::LBrace => {
                let ty = self.simple_type()?;
                pt.extend(name, ty);
            }
            Token::Colon => {
                self.advance();
                let e = self.expr()?;
                filters.push((name, e));
            }
            t => {
                return Err(format!(
                    "expected '::', 'TYPED', type, or ':' after record key, got {t:?}"
                ))
            }
        }
        Ok(())
    }

    fn simple_type(&mut self) -> Result<SimpleType, String> {
        match self.advance() {
            Token::Int => Ok(SimpleType::Z),
            Token::Float => Ok(SimpleType::F),
            Token::Bool => Ok(SimpleType::B),
            Token::Str => Ok(SimpleType::S),
            Token::Any | Token::Star => Ok(SimpleType::Star),
            Token::LBracket => {
                let inner = self.simple_type()?;
                self.expect(&Token::RBracket)?;
                Ok(SimpleType::List(Box::new(inner)))
            }
            Token::LBrace => {
                // Record type `{k T, ...}` / `{k :: T, ...}` / `{k TYPED T, ...}`.
                // ISO GQL: `:` is reserved for values, so type-record fields use
                // the `<typed>` element (optional `::` or `TYPED`) or implicit form.
                let mut fields = std::collections::BTreeMap::new();
                if self.eat(&Token::RBrace) {
                    return Ok(SimpleType::Record(fields));
                }
                loop {
                    let k = match self.advance() {
                        Token::Name(n) => n,
                        t => return Err(format!("expected field name, got {t:?}")),
                    };
                    if matches!(self.peek(), Token::DoubleColon | Token::Typed) {
                        self.advance();
                    }
                    fields.insert(k, self.simple_type()?);
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                self.expect(&Token::RBrace)?;
                Ok(SimpleType::Record(fields))
            }
            t => Err(format!(
                "expected type (int/float/bool/str/*/[T]/{{k T}}), got {t:?}"
            )),
        }
    }

    // ===== Expressions =====

    fn expr(&mut self) -> Result<Expr, String> {
        self.logical_op()
    }

    // logical_op = comparison (("and"|"or") comparison)*
    fn logical_op(&mut self) -> Result<Expr, String> {
        let mut left = self.comparison()?;
        loop {
            let op = match self.peek() {
                Token::And => BinOp::And,
                Token::Or => BinOp::Or,
                _ => break,
            };
            self.advance();
            let right = self.comparison()?;
            left = Expr::Binop {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // comparison = term (cop term)*
    fn comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        loop {
            // SQL-style null tests: `is null` / `is not null`. Detected
            // before the generic `is type` branch so `null` is not parsed
            // as an ordinary type expression.
            if let Some((operand, negated)) = self.try_is_null(&left) {
                left = Expr::IsNull {
                    operand: Box::new(operand),
                    negated,
                };
                continue;
            }
            let op = match self.peek() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::Le => BinOp::Le,
                Token::Ge => BinOp::Ge,
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Typed => BinOp::Is,
                // Implicit type predicate: a type-head token after a term.
                Token::Int
                | Token::Float
                | Token::Bool
                | Token::Str
                | Token::Any
                | Token::Star
                | Token::LBracket
                | Token::LBrace => {
                    let right = self.term()?;
                    left = Expr::Binop {
                        op: BinOp::Is,
                        left: Box::new(left),
                        right: Box::new(right),
                    };
                    continue;
                }
                Token::As => BinOp::As,
                Token::In => BinOp::In,
                _ => break,
            };
            self.advance();
            let right = self.term()?;
            left = Expr::Binop {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // term = factor (("+" | "-") factor)*  — additive level.
    fn term(&mut self) -> Result<Expr, String> {
        let mut left = self.factor()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.factor()?;
            left = Expr::Binop {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // factor = unary (("*" | "/") unary)*  — multiplicative level binds
    // tighter than +/-. `*` (Token::Star) is otherwise a type wildcard
    // (`x is *`) and the `count(*)` argument, but neither reaches
    // value-expression position via this path, so consuming it here as
    // multiplication is safe. `/` (Token::Slash) is the ISO <solidus>.
    fn factor(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            left = Expr::Binop {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // unary = "-" unary | "not" unary | primary
    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat(&Token::Minus) {
            let operand = self.unary()?;
            return Ok(Expr::Unop {
                op: UnOp::Neg,
                operand: Box::new(operand),
            });
        }
        if self.eat(&Token::Not) {
            // `NOT EXISTS { ... }` is a distinct AST variant from
            // `NOT (EXISTS { ... })` — the optimiser folds it to `true`
            // when the body is empty (vs `false` for `EXISTS`).
            if self.eat(&Token::Exists) {
                let body = self.parse_exists_body()?;
                return Ok(Expr::NotExists {
                    body: Box::new(body),
                });
            }
            let operand = self.unary()?;
            return Ok(Expr::Unop {
                op: UnOp::Not,
                operand: Box::new(operand),
            });
        }
        if self.eat(&Token::Exists) {
            let body = self.parse_exists_body()?;
            return Ok(Expr::Exists {
                body: Box::new(body),
            });
        }
        self.primary_expr()
    }

    /// Parse the body of an `EXISTS { ... }` or `NOT EXISTS { ... }`
    /// subquery: one or more match clauses (with the leading `MATCH`
    /// optional, as in the top-level grammar). RETURN, GROUP BY,
    /// LIMIT, and DISTINCT are not allowed inside the braces — the
    /// body's purpose is proving non-emptiness, not producing a
    /// projected result table.
    fn parse_exists_body(&mut self) -> Result<Query, String> {
        self.expect(&Token::LBrace)?;

        let first_optional = self.eat(&Token::Optional);
        if first_optional {
            self.expect(&Token::Match)?;
        } else {
            self.eat(&Token::Match);
        }
        let first_stmt = self.match_statement(first_optional)?;
        let mut matches = vec![first_stmt];

        loop {
            if self.eat(&Token::Optional) {
                self.expect(&Token::Match)?;
                matches.push(self.match_statement(true)?);
            } else if self.eat(&Token::Match) {
                matches.push(self.match_statement(false)?);
            } else {
                break;
            }
        }

        if self.check(&Token::Return) {
            return Err("EXISTS body cannot contain RETURN".into());
        }
        if self.check(&Token::GroupBy) {
            return Err("EXISTS body cannot contain GROUP BY".into());
        }
        if self.check(&Token::OrderBy) {
            return Err("EXISTS body cannot contain ORDER BY".into());
        }
        if self.check(&Token::Limit) {
            return Err("EXISTS body cannot contain LIMIT".into());
        }

        self.expect(&Token::RBrace)?;

        Ok(Query {
            matches,
            group_by: None,
            returns: None,
            distinct: false,
            order_by: None,
            limit: None,
        })
    }

    /// Parse a brace block in expression position, after the optional
    /// `RECORD` keyword has been consumed (the `{` has NOT). ISO GQL:
    /// `:` introduces a value field, the `<typed>` element (implicit,
    /// `::`, `TYPED`) introduces a type field. The token after the first
    /// field name picks the form:
    ///   `{k : v, ...}`      → record value (values are expressions)
    ///   `{k T, ...}`        → record type, implicit
    ///   `{k :: T, ...}`     → record type, explicit
    ///   `{k TYPED T, ...}`  → record type, keyword
    /// A fully-constant value record folds to `Expr::Const(Value::Record)`
    /// so equality and storage round-trips keep working; otherwise it
    /// becomes a dynamic `Expr::Record { fields }`.
    fn parse_brace_record(&mut self) -> Result<Expr, String> {
        self.expect(&Token::LBrace)?;
        if self.eat(&Token::RBrace) {
            return Ok(Expr::Const(
                Value::Record(std::collections::BTreeMap::new()),
            ));
        }
        let saved = self.pos;
        let is_value_record = matches!(
            (self.peek_at(0), self.peek_at(1)),
            (Some(Token::Name(_)), Some(Token::Colon))
        );
        if is_value_record {
            let mut fields: Vec<(String, Expr)> = Vec::new();
            loop {
                let k = match self.advance() {
                    Token::Name(n) => n,
                    t => return Err(format!("expected field name, got {t:?}")),
                };
                self.expect(&Token::Colon)?;
                fields.push((k, self.expr()?));
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::RBrace)?;
            // Const fast-path: a record of only literals folds to a Value
            // so PartialEq / `in` / storage round-trips keep working.
            if fields.iter().all(|(_, e)| matches!(e, Expr::Const(_))) {
                let mut m = std::collections::BTreeMap::new();
                for (k, e) in fields {
                    if let Expr::Const(v) = e {
                        m.insert(k, v);
                    }
                }
                return Ok(Expr::Const(Value::Record(m)));
            }
            return Ok(Expr::Record { fields });
        }
        self.pos = saved;
        let mut type_fields: std::collections::BTreeMap<String, SimpleType> =
            std::collections::BTreeMap::new();
        loop {
            let k = match self.advance() {
                Token::Name(n) => n,
                t => return Err(format!("expected field name, got {t:?}")),
            };
            if matches!(self.peek(), Token::DoubleColon | Token::Typed) {
                self.advance();
            }
            type_fields.insert(k, self.simple_type()?);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(Expr::Type(SimpleType::Record(type_fields)))
    }

    /// Parse the body of a `VALUE { ... }` value query expression: a
    /// match chain (leading MATCH optional, as elsewhere), then a
    /// mandatory RETURN with **exactly one** item, optional ORDER BY and
    /// LIMIT. GROUP BY and DISTINCT are rejected — the body projects a
    /// single value, not a grouped/deduplicated table.
    fn parse_value_body(&mut self) -> Result<Query, String> {
        self.expect(&Token::LBrace)?;

        let first_optional = self.eat(&Token::Optional);
        if first_optional {
            self.expect(&Token::Match)?;
        } else {
            self.eat(&Token::Match);
        }
        let first_stmt = self.match_statement(first_optional)?;
        let matches = self.continue_match_chain(vec![first_stmt])?;

        if self.check(&Token::GroupBy) {
            return Err("VALUE subquery body cannot contain GROUP BY".into());
        }
        if !self.eat(&Token::Return) {
            return Err("VALUE subquery body requires a RETURN clause".into());
        }
        if self.eat(&Token::Distinct) {
            return Err("VALUE subquery body cannot use DISTINCT".into());
        }
        let items = self.return_list()?;
        if items.len() != 1 {
            return Err(format!(
                "VALUE subquery must project exactly one item, got {}",
                items.len()
            ));
        }
        if self.check(&Token::GroupBy) {
            return Err("VALUE subquery body cannot contain GROUP BY".into());
        }
        let order_by = self.parse_optional_order_by(Some(&items))?;
        let limit = self.parse_optional_limit()?;
        self.expect(&Token::RBrace)?;

        Ok(Query {
            matches,
            group_by: None,
            returns: Some(items),
            distinct: false,
            order_by,
            limit,
        })
    }

    // primary = aggregate_function | constant | list_literal | attr_lookup
    //         | simple_type | "(" expr ")"
    fn primary_expr(&mut self) -> Result<Expr, String> {
        // An aggregate call (`COUNT(...)`, `SUM(...)`, ...) is a primary so
        // it can be an operand of arithmetic: `COUNT(x) + COUNT(y)`. A bare
        // top-level aggregate in RETURN is re-folded into a
        // `ReturnItem::Aggregate` by `return_item` for backward compat.
        if self.peek_aggregate_kind().is_some() {
            let agg = self.aggregate_function()?;
            return Ok(Expr::Agg(Box::new(agg)));
        }
        match self.peek().clone() {
            Token::Number(n) => {
                self.advance();
                Ok(Expr::Const(Value::Int(n)))
            }
            Token::FloatLit(x) => {
                self.advance();
                Ok(Expr::Const(Value::Float(x)))
            }
            Token::StringLit(s) => {
                self.advance();
                Ok(Expr::Const(Value::Str(s)))
            }
            Token::LBracket => {
                // Two forms share the `[` token in expression position:
                //   list type `[T]`     — right operand of `is`/`as` (becomes Expr::Type)
                //   list value `[e, e]` — anywhere values go             (becomes Expr::Const)
                // Try to parse as a type; if that fails cleanly, rewind and parse as a
                // list value literal. This is how we support arbitrarily nested `[[T]]`
                // without hand-rolled lookahead tables.
                self.advance();
                if self.eat(&Token::RBracket) {
                    return Ok(Expr::Const(Value::List(Vec::new())));
                }
                // List comprehension `[<var> IN <source> [WHERE <f>] | <body>]`.
                // Distinguished from a list literal whose first element is an
                // `IN` expression (`[a IN b, ...]`) by the `|` separator: we
                // try the comprehension and rewind if no `|`/`WHERE` follows.
                let saved = self.pos;
                if matches!(self.peek(), Token::Name(_))
                    && matches!(self.peek_at(1), Some(Token::In))
                {
                    if let Some(expr) = self.try_list_comprehension()? {
                        return Ok(expr);
                    }
                    self.pos = saved;
                }
                let saved = self.pos;
                if let Ok(inner) = self.simple_type() {
                    if self.eat(&Token::RBracket) {
                        return Ok(Expr::Type(SimpleType::List(Box::new(inner))));
                    }
                }
                self.pos = saved;
                let mut items: Vec<Expr> = vec![self.expr()?];
                while self.eat(&Token::Comma) {
                    items.push(self.expr()?);
                }
                self.expect(&Token::RBracket)?;
                // Fold into Value::List when every element is a constant so equality
                // and `in` work via PartialEq. Non-constant elements are rejected for
                // now; a follow-up can add Expr::ListLit for dynamic lists.
                let mut consts = Vec::with_capacity(items.len());
                let mut all_const = true;
                for e in &items {
                    if let Expr::Const(v) = e {
                        consts.push(v.clone());
                    } else {
                        all_const = false;
                        break;
                    }
                }
                if all_const {
                    Ok(Expr::Const(Value::List(consts)))
                } else {
                    Err("non-constant list literal elements are not supported yet".into())
                }
            }
            Token::True => {
                self.advance();
                Ok(Expr::Const(Value::Bool(true)))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Const(Value::Bool(false)))
            }
            Token::Null => {
                self.advance();
                Ok(Expr::Const(Value::Null))
            }
            Token::Int => {
                self.advance();
                Ok(Expr::Type(SimpleType::Z))
            }
            Token::Float => {
                self.advance();
                Ok(Expr::Type(SimpleType::F))
            }
            Token::Bool => {
                self.advance();
                Ok(Expr::Type(SimpleType::B))
            }
            Token::Str => {
                self.advance();
                Ok(Expr::Type(SimpleType::S))
            }
            Token::Any | Token::Star => {
                self.advance();
                Ok(Expr::Type(SimpleType::Star))
            }
            Token::Name(name)
                if name.eq_ignore_ascii_case("CASE") && self.peek_name_keyword(1, "WHEN") =>
            {
                self.case_expr()
            }
            Token::Name(name)
                if name.eq_ignore_ascii_case("MOD")
                    && matches!(self.peek_at(1), Some(Token::LParen)) =>
            {
                self.mod_expr()
            }
            Token::Name(name) => {
                self.advance();
                // ISO §20.16 path functions (`ELEMENTS`/`PATH_LENGTH`/
                // `CARDINALITY`, plus the non-standard `NODES`/`EDGES`
                // translation helpers) are soft keywords: only a call form
                // `NAME(<expr>)` is special. A bare `NAME` stays a variable
                // reference, so these names remain usable as variables and
                // labels everywhere else.
                if matches!(self.peek(), Token::LParen) {
                    if let Some(canon) = path_function_name(&name) {
                        self.advance(); // consume '('
                        let arg = self.expr()?;
                        self.expect(&Token::RParen)?;
                        return Ok(Expr::Call {
                            name: canon,
                            args: vec![arg],
                        });
                    }
                }
                // First dot: variable-to-property. Subsequent dots: field access
                // on the previous value (for nested records).
                if self.eat(&Token::Dot) {
                    let attr = match self.advance() {
                        Token::Name(a) => a,
                        t => return Err(format!("expected attribute name after '.', got {t:?}")),
                    };
                    let mut expr = Expr::AttrLookup { var: name, attr };
                    while self.eat(&Token::Dot) {
                        let field = match self.advance() {
                            Token::Name(a) => a,
                            t => return Err(format!("expected field name after '.', got {t:?}")),
                        };
                        expr = Expr::FieldAccess {
                            base: Box::new(expr),
                            field,
                        };
                    }
                    Ok(expr)
                } else {
                    // ISO §20.12 <binding variable reference>: a bare
                    // name in expression position resolves to the
                    // variable's value at runtime (a node / edge
                    // reference value, or the projected column when
                    // used as a sort key).
                    Ok(Expr::Var(name))
                }
            }
            Token::LParen => {
                self.advance();
                let e = self.expr()?;
                self.expect(&Token::RParen)?;
                Ok(e)
            }
            Token::Coalesce => {
                // ISO §20.7: at least two args.
                self.advance();
                self.expect(&Token::LParen)?;
                let mut args = vec![self.expr()?];
                while self.eat(&Token::Comma) {
                    args.push(self.expr()?);
                }
                self.expect(&Token::RParen)?;
                if args.len() < 2 {
                    return Err("COALESCE requires at least two arguments per ISO §20.7".into());
                }
                Ok(Expr::Coalesce(args))
            }
            Token::Duration => {
                // `DURATION({unit: expr, ...})` desugars to an integer
                // count of milliseconds: `Σ expr_i * ms_per_unit_i`. The
                // operands stay symbolic (e.g. `$durationDays`), so the
                // whole thing is plain int arithmetic the runtime already
                // evaluates. Only unambiguous calendar-free units are
                // supported (years/months have no fixed ms length).
                self.advance();
                self.expect(&Token::LParen)?;
                self.expect(&Token::LBrace)?;
                let mut total: Option<Expr> = None;
                loop {
                    let unit = match self.advance() {
                        Token::Name(n) => n,
                        t => return Err(format!("expected duration unit name, got {t:?}")),
                    };
                    let ms: i64 = match unit.to_ascii_lowercase().as_str() {
                        "weeks" | "week" => 604_800_000,
                        "days" | "day" => 86_400_000,
                        "hours" | "hour" => 3_600_000,
                        "minutes" | "minute" => 60_000,
                        "seconds" | "second" => 1_000,
                        "milliseconds" | "millisecond" | "millis" => 1,
                        other => {
                            return Err(format!(
                                "unsupported DURATION unit '{other}' (use weeks/days/hours/minutes/seconds/milliseconds)"
                            ))
                        }
                    };
                    self.expect(&Token::Colon)?;
                    let amount = self.expr()?;
                    let term = Expr::Binop {
                        op: BinOp::Mul,
                        left: Box::new(amount),
                        right: Box::new(Expr::Const(Value::Int(ms))),
                    };
                    total = Some(match total {
                        None => term,
                        Some(acc) => Expr::Binop {
                            op: BinOp::Add,
                            left: Box::new(acc),
                            right: Box::new(term),
                        },
                    });
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                self.expect(&Token::RBrace)?;
                self.expect(&Token::RParen)?;
                total.ok_or_else(|| "DURATION requires at least one unit field".to_string())
            }
            Token::Floor => {
                // ISO <floor function>: FLOOR(<numeric value expression>).
                self.advance();
                self.expect(&Token::LParen)?;
                let arg = self.expr()?;
                self.expect(&Token::RParen)?;
                Ok(Expr::Call {
                    name: "FLOOR".into(),
                    args: vec![arg],
                })
            }
            Token::Cast => {
                // ISO <cast specification>: CAST(<operand> AS <value type>).
                // Restricted to INTEGER / FLOAT targets (the conversions the
                // runtime implements); the target rides as a type argument.
                self.advance();
                self.expect(&Token::LParen)?;
                // Parse the operand with the AS-excluding comparison rule so
                // the `AS <value type>` separator is not swallowed as the
                // type-assertion operator.
                let operand = self.return_comparison()?;
                self.expect(&Token::As)?;
                let target = self.simple_type()?;
                if !matches!(target, SimpleType::Z | SimpleType::F) {
                    return Err(format!(
                        "CAST target must be INTEGER or FLOAT, got {target}"
                    ));
                }
                self.expect(&Token::RParen)?;
                Ok(Expr::Call {
                    name: "CAST".into(),
                    args: vec![operand, Expr::Type(target)],
                })
            }
            Token::Record => {
                // `RECORD { ... }` — explicit constructor keyword. Falls
                // through to the same brace parsing as a bare `{ ... }`.
                self.advance();
                self.parse_brace_record()
            }
            Token::LBrace => self.parse_brace_record(),
            Token::Value => {
                // ISO <value query expression>: VALUE { <nested query> }.
                self.advance();
                let body = self.parse_value_body()?;
                Ok(Expr::ValueSubquery {
                    body: Box::new(body),
                })
            }
            _ => Err(format!("expected expression, got {:?}", self.peek())),
        }
    }

    fn case_expr(&mut self) -> Result<Expr, String> {
        self.expect_name_keyword("CASE")?;
        let mut branches = Vec::new();
        while self.eat_name_keyword("WHEN") {
            let cond = self.expr()?;
            self.expect_name_keyword("THEN")?;
            let value = self.expr()?;
            branches.push((cond, value));
        }
        if branches.is_empty() {
            return Err("CASE requires at least one WHEN branch".into());
        }
        let else_expr = if self.eat_name_keyword("ELSE") {
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        self.expect_name_keyword("END")?;
        Ok(Expr::Case {
            branches,
            else_expr,
        })
    }

    /// Parse a list comprehension body after the opening `[` has been
    /// consumed and the caller has verified the next two tokens are
    /// `<Name> IN`. Returns `Ok(None)` (so the caller can rewind and try a
    /// list literal) when the `<var> IN <source>` head is not followed by a
    /// `WHERE` filter or a `|` body separator — i.e. it was an `IN`
    /// expression inside a plain list literal, not a comprehension.
    fn try_list_comprehension(&mut self) -> Result<Option<Expr>, String> {
        let var = match self.advance() {
            Token::Name(n) => n,
            _ => return Ok(None),
        };
        if !self.eat(&Token::In) {
            return Ok(None);
        }
        let source = Box::new(self.expr()?);
        let filter = if self.eat(&Token::Where) {
            // A `WHERE` commits us to the comprehension form.
            Some(Box::new(self.expr()?))
        } else if !self.eat(&Token::Pipe) {
            // No filter and no `|`: this was a list literal element.
            return Ok(None);
        } else {
            // `|` already consumed above for the no-filter case.
            let body = Box::new(self.expr()?);
            self.expect(&Token::RBracket)?;
            return Ok(Some(Expr::ListComprehension {
                var,
                source,
                filter: None,
                body,
            }));
        };
        self.expect(&Token::Pipe)?;
        let body = Box::new(self.expr()?);
        self.expect(&Token::RBracket)?;
        Ok(Some(Expr::ListComprehension {
            var,
            source,
            filter,
            body,
        }))
    }

    fn mod_expr(&mut self) -> Result<Expr, String> {
        self.expect_name_keyword("MOD")?;
        self.expect(&Token::LParen)?;
        let left = self.expr()?;
        self.expect(&Token::Comma)?;
        let right = self.expr()?;
        self.expect(&Token::RParen)?;
        Ok(Expr::Binop {
            op: BinOp::Mod,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    // ===== Top-level statement: query or DDL =====

    fn statement(&mut self) -> Result<Statement, String> {
        match self.peek() {
            Token::Create => self.create_statement(),
            Token::Use => self.use_graph_type(),
            Token::Drop => self.drop_statement(),
            Token::Show => self.show_statement(),
            Token::Validate => self.validate_graph_type(),
            // Standalone INSERT / SET / REMOVE (no MATCH chain). SET
            // and REMOVE without a MATCH usually have nothing to bind,
            // but parsing still succeeds — the runtime runs zero
            // iterations and reports counts of zero.
            Token::Insert | Token::Set | Token::Remove => Ok(Statement::DataModification(
                self.parse_dm_after_matches(Vec::new())?,
            )),
            // DETACH/NODETACH/DELETE alone are illegal: ISO §13.5 SR3
            // requires the working table provided by a preceding MATCH.
            Token::Detach | Token::NoDetach | Token::Delete => Err(format!(
                "{:?} requires a preceding MATCH clause (§13.5)",
                self.peek()
            )),
            // Either a normal Query or a MATCH-prefixed DM. Disambiguate
            // by parsing the match chain first and then peeking.
            Token::Match | Token::Optional => {
                let matches = self.parse_match_chain_explicit()?;
                match self.peek() {
                    Token::Insert
                    | Token::Set
                    | Token::Remove
                    | Token::Detach
                    | Token::NoDetach
                    | Token::Delete => Ok(Statement::DataModification(
                        self.parse_dm_after_matches(matches)?,
                    )),
                    _ => {
                        let q = self.finish_query_after_matches(matches)?;
                        Ok(Statement::Query(q))
                    }
                }
            }
            _ => {
                let q = self.full_query()?;
                Ok(Statement::Query(q))
            }
        }
    }

    /// Parse the DML op + optional RETURN + LIMIT, given an already-parsed
    /// MATCH chain (possibly empty for standalone INSERT). The next token
    /// must be one of INSERT / DETACH / NODETACH / DELETE.
    fn parse_dm_after_matches(
        &mut self,
        matches: Vec<MatchStatement>,
    ) -> Result<DmStatement, String> {
        let op = match self.peek() {
            Token::Insert => self.parse_insert_op()?,
            Token::Set => self.parse_set_op()?,
            Token::Remove => self.parse_remove_op()?,
            Token::Detach | Token::NoDetach | Token::Delete => self.parse_delete_op()?,
            t => return Err(format!("expected INSERT, SET, REMOVE or DELETE, got {t:?}")),
        };
        // ISO §14.10 optional RETURN trailing the DM.
        let (returns, _distinct) = if self.eat(&Token::Return) {
            // MVP-0: DISTINCT in DML RETURN is rare; accept it but discard.
            let distinct = self.eat(&Token::Distinct);
            let items = self.return_list()?;
            (Some(items), distinct)
        } else {
            (None, false)
        };
        let limit = self.parse_optional_limit()?;
        Ok(DmStatement {
            matches,
            op,
            returns,
            limit,
        })
    }

    /// `INSERT <insert path pattern list>` — ISO §13.2 + §16.5.
    fn parse_insert_op(&mut self) -> Result<DmOp, String> {
        self.expect(&Token::Insert)?;
        let mut patterns = vec![self.parse_insert_path_pattern()?];
        while self.eat(&Token::Comma) {
            patterns.push(self.parse_insert_path_pattern()?);
        }
        Ok(DmOp::Insert(patterns))
    }

    /// One `<insert path pattern>` — alternating insert nodes and edges.
    /// We reuse the regular path-pattern parser and validate afterwards
    /// that no MATCH-only constructs (filters, repetitions, unions, etc.)
    /// snuck in. ISO §16.5 keeps INSERT patterns as a strict subset.
    fn parse_insert_path_pattern(&mut self) -> Result<PathPattern, String> {
        // Parse a single concat path (no top-level comma — that's the
        // outer `<insert path pattern list>` separator).
        let pattern = self.path_pattern()?;
        validate_insert_pattern(&pattern)?;
        Ok(pattern)
    }

    /// `[ DETACH | NODETACH ] DELETE <value expression list>` —
    /// ISO §13.5. MVP-1.E enables Feature GD04 ("simple expression
    /// support"): each target is now any `<value expression>` (e.g.
    /// `n`, `n.parent`, `coalesce(a, b)`) and gets evaluated per
    /// binding row at runtime. NODETACH is the implicit default per
    /// §13.5 SR6.
    fn parse_delete_op(&mut self) -> Result<DmOp, String> {
        let detach = if self.eat(&Token::Detach) {
            true
        } else if self.eat(&Token::NoDetach) {
            false
        } else {
            // No DETACH/NODETACH prefix → NODETACH per §13.5 SR6.
            false
        };
        self.expect(&Token::Delete)?;
        let mut targets = vec![self.expr()?];
        while self.eat(&Token::Comma) {
            targets.push(self.expr()?);
        }
        Ok(DmOp::Delete { detach, targets })
    }

    /// `SET <set item list>` — ISO §13.3. MVP-1.B handles property and
    /// all-properties items; the label form (`SET x:Label`) belongs to
    /// MVP-1.D and is rejected here.
    fn parse_set_op(&mut self) -> Result<DmOp, String> {
        self.expect(&Token::Set)?;
        let mut items = vec![self.parse_set_item()?];
        while self.eat(&Token::Comma) {
            items.push(self.parse_set_item()?);
        }
        Ok(DmOp::Set(items))
    }

    fn parse_set_item(&mut self) -> Result<SetItem, String> {
        let var = self.expect_var_name("SET")?;
        if self.eat(&Token::Eq) {
            // <set all properties item>: x = { props }
            self.expect(&Token::LBrace)?;
            let props = if self.check(&Token::RBrace) {
                Vec::new()
            } else {
                self.parse_pkv_list()?
            };
            self.expect(&Token::RBrace)?;
            return Ok(SetItem::AllProperties { var, props });
        }
        if self.eat(&Token::Dot) {
            // <set property item>: x.prop = value
            let prop = match self.advance() {
                Token::Name(n) => n,
                t => return Err(format!("SET: expected property name after '.', got {t:?}")),
            };
            self.expect(&Token::Eq)?;
            let value = self.expr()?;
            return Ok(SetItem::Property { var, prop, value });
        }
        // <set label item>: `x:Label` or `x IS Label`. ISO §13.3 GR8 c,
        // Feature GD02. The lexer maps both `is` and `IS` to `Typed`, so
        // the same arm handles either spelling.
        if self.eat(&Token::Colon) || self.eat(&Token::Typed) {
            let label = match self.advance() {
                Token::Name(n) => n,
                t => return Err(format!("SET: expected label name, got {t:?}")),
            };
            return Ok(SetItem::Label { var, label });
        }
        Err(format!(
            "SET: expected '=', '.<prop> = value', ':Label', or 'IS Label' after '{var}'"
        ))
    }

    /// `REMOVE <remove item list>` — ISO §13.4. MVP-1.C handles
    /// `<remove property item>`; the label form lands in MVP-1.D.
    fn parse_remove_op(&mut self) -> Result<DmOp, String> {
        self.expect(&Token::Remove)?;
        let mut items = vec![self.parse_remove_item()?];
        while self.eat(&Token::Comma) {
            items.push(self.parse_remove_item()?);
        }
        Ok(DmOp::Remove(items))
    }

    fn parse_remove_item(&mut self) -> Result<RemoveItem, String> {
        let var = self.expect_var_name("REMOVE")?;
        if self.eat(&Token::Dot) {
            let prop = match self.advance() {
                Token::Name(n) => n,
                t => {
                    return Err(format!(
                        "REMOVE: expected property name after '.', got {t:?}"
                    ))
                }
            };
            return Ok(RemoveItem::Property { var, prop });
        }
        // <remove label item>: `x:Label` or `x IS Label`. ISO §13.4 GR4
        // b, Feature GD02. Idempotent — removing a label the element does
        // not carry is a no-op.
        if self.eat(&Token::Colon) || self.eat(&Token::Typed) {
            let label = match self.advance() {
                Token::Name(n) => n,
                t => return Err(format!("REMOVE: expected label name, got {t:?}")),
            };
            return Ok(RemoveItem::Label { var, label });
        }
        Err(format!(
            "REMOVE: expected '.<prop>', ':Label', or 'IS Label' after '{var}'"
        ))
    }

    /// `<property key value pair list>` — comma-separated `name: expr`.
    /// Caller is expected to have eaten the opening `{` and to eat the
    /// closing `}` itself; this helper just walks the contents.
    fn parse_pkv_list(&mut self) -> Result<Vec<(String, Expr)>, String> {
        let mut out = Vec::new();
        loop {
            let name = match self.advance() {
                Token::Name(n) => n,
                t => return Err(format!("expected property name, got {t:?}")),
            };
            self.expect(&Token::Colon)?;
            let value = self.expr()?;
            out.push((name, value));
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(out)
    }

    /// Helper: consume one `Token::Name` and return its string.
    /// MVP-0 keeps DELETE targets to bare names (§13.5 GD04 disabled).
    fn expect_var_name(&mut self, ctx: &str) -> Result<String, String> {
        match self.advance() {
            Token::Name(n) => Ok(n),
            t => Err(format!(
                "{ctx}: expected variable name, got {t:?} (Feature GD04 \
                 'simple expression support' is not enabled in this MVP)"
            )),
        }
    }

    /// Dispatch CREATE → GRAPH TYPE or [HASH|BTREE] INDEX. The lookahead
    /// after CREATE determines which: `GRAPH`, an explicit kind keyword
    /// (`HASH` / `BTREE`), or `INDEX` directly.
    fn create_statement(&mut self) -> Result<Statement, String> {
        // Tokens consumed inside the helpers; we only peek here.
        let pos = self.pos; // remember in case we need to rewind
        self.expect(&Token::Create)?;
        let after = self.peek().clone();
        // Restore — the helpers expect to see CREATE first.
        self.pos = pos;
        match after {
            Token::Graph => self.create_graph_type(),
            Token::Hash | Token::BTree | Token::Index => self.create_index(),
            t => Err(format!(
                "after CREATE, expected GRAPH or INDEX (with optional HASH/BTREE), got {t:?}"
            )),
        }
    }

    fn drop_statement(&mut self) -> Result<Statement, String> {
        let pos = self.pos;
        self.expect(&Token::Drop)?;
        let after = self.peek().clone();
        self.pos = pos;
        match after {
            Token::Graph => self.drop_graph_type(),
            Token::Index => self.drop_index(),
            t => Err(format!("after DROP, expected GRAPH or INDEX, got {t:?}")),
        }
    }

    fn show_statement(&mut self) -> Result<Statement, String> {
        // Peek past SHOW to dispatch.
        let pos = self.pos;
        self.expect(&Token::Show)?;
        let after = self.peek().clone();
        self.pos = pos;
        match after {
            Token::Indexes => {
                self.expect(&Token::Show)?;
                self.expect(&Token::Indexes)?;
                Ok(Statement::ShowIndexes)
            }
            _ => self.show_graph_type(),
        }
    }

    /// `SHOW GRAPH TYPES` | `SHOW GRAPH TYPE <name>` | `SHOW CURRENT GRAPH TYPE`
    fn show_graph_type(&mut self) -> Result<Statement, String> {
        self.expect(&Token::Show)?;
        if self.eat(&Token::Current) {
            self.expect_graph_type_kw()?;
            return Ok(Statement::ShowCurrentGraphType);
        }
        self.expect(&Token::Graph)?;
        if self.eat(&Token::Types) {
            return Ok(Statement::ShowGraphTypes);
        }
        self.expect(&Token::TypeKw)?;
        let (name, _is_default) = self.graph_type_name_or_default()?;
        Ok(Statement::ShowGraphType { name })
    }

    /// `VALIDATE GRAPH TYPE <name>` — explicit data-vs-schema check.
    /// `DEFAULT` is allowed: it's always derived from data, but the
    /// command is still useful as a no-op sanity check.
    fn validate_graph_type(&mut self) -> Result<Statement, String> {
        self.expect(&Token::Validate)?;
        self.expect_graph_type_kw()?;
        let (name, _is_default) = self.graph_type_name_or_default()?;
        Ok(Statement::ValidateGraphType { name })
    }

    fn expect_graph_type_kw(&mut self) -> Result<(), String> {
        self.expect(&Token::Graph)?;
        self.expect(&Token::TypeKw)?;
        Ok(())
    }

    /// `CREATE GRAPH TYPE <name> AS { <type-elements> }`
    fn create_graph_type(&mut self) -> Result<Statement, String> {
        self.expect(&Token::Create)?;
        self.expect_graph_type_kw()?;
        let name = self.graph_type_name()?;
        if name.eq_ignore_ascii_case("DEFAULT") {
            return Err("DEFAULT is a reserved graph type name".into());
        }
        self.expect(&Token::As)?;
        let body = self.type_body()?;
        Ok(Statement::CreateGraphType { name, body })
    }

    /// `USE GRAPH TYPE <name>`. `DEFAULT` flips `refresh_default` so the
    /// handler re-runs schema inference instead of reusing the stored copy.
    fn use_graph_type(&mut self) -> Result<Statement, String> {
        self.expect(&Token::Use)?;
        self.expect_graph_type_kw()?;
        let (name, is_default) = self.graph_type_name_or_default()?;
        Ok(Statement::UseGraphType {
            name,
            refresh_default: is_default,
        })
    }

    /// `DROP GRAPH TYPE <name>`. Reject DEFAULT at parse time so callers
    /// don't need to repeat the check.
    fn drop_graph_type(&mut self) -> Result<Statement, String> {
        self.expect(&Token::Drop)?;
        self.expect_graph_type_kw()?;
        let (name, is_default) = self.graph_type_name_or_default()?;
        if is_default {
            return Err("DEFAULT is a reserved graph type and cannot be dropped".into());
        }
        Ok(Statement::DropGraphType { name })
    }

    /// `CREATE [HASH | BTREE] INDEX [<name>] ON :Label(prop) [USING HASH | BTREE]`
    ///
    /// Both prefix (`CREATE BTREE INDEX foo ON :Label(prop)`) and suffix
    /// (`CREATE INDEX foo ON :Label(prop) USING BTREE`) syntaxes are
    /// accepted; if both are given they must agree. `name` is optional
    /// (the handler auto-generates `<label>_<prop>_<kind>` if omitted).
    fn create_index(&mut self) -> Result<Statement, String> {
        self.expect(&Token::Create)?;
        // Prefix kind keyword.
        let prefix_kind = match self.peek() {
            Token::Hash => {
                self.advance();
                Some(IndexKindStmt::Hash)
            }
            Token::BTree => {
                self.advance();
                Some(IndexKindStmt::BTree)
            }
            _ => None,
        };
        self.expect(&Token::Index)?;

        // Optional bare name. We tell name from `ON` by token type.
        let name = match self.peek() {
            Token::Name(_) => match self.advance() {
                Token::Name(n) => Some(n),
                _ => unreachable!(),
            },
            _ => None,
        };

        self.expect(&Token::On)?;
        // `:Label(prop)`
        self.expect(&Token::Colon)?;
        let label = match self.advance() {
            Token::Name(n) => n,
            t => return Err(format!("expected label name, got {t:?}")),
        };
        self.expect(&Token::LParen)?;
        let prop = match self.advance() {
            Token::Name(n) => n,
            t => return Err(format!("expected property name, got {t:?}")),
        };
        self.expect(&Token::RParen)?;

        // Optional `USING <kind>` suffix.
        let suffix_kind = if self.eat(&Token::Using) {
            match self.advance() {
                Token::Hash => Some(IndexKindStmt::Hash),
                Token::BTree => Some(IndexKindStmt::BTree),
                t => return Err(format!("expected HASH or BTREE after USING, got {t:?}")),
            }
        } else {
            None
        };

        let kind = match (prefix_kind, suffix_kind) {
            (Some(a), Some(b)) if a != b => {
                return Err("conflicting index kinds: prefix says one, USING says another".into());
            }
            (Some(k), _) | (_, Some(k)) => k,
            (None, None) => IndexKindStmt::Hash,
        };

        Ok(Statement::CreateIndex {
            name,
            label,
            prop,
            kind,
        })
    }

    /// `DROP INDEX <name>`
    fn drop_index(&mut self) -> Result<Statement, String> {
        self.expect(&Token::Drop)?;
        self.expect(&Token::Index)?;
        let name = match self.advance() {
            Token::Name(n) => n,
            t => return Err(format!("expected index name, got {t:?}")),
        };
        Ok(Statement::DropIndex { name })
    }

    /// Bare graph-type name in a CREATE position. Rejects the reserved
    /// `DEFAULT` keyword (callers like `use_graph_type` use the variant
    /// `graph_type_name_or_default` that allows it).
    fn graph_type_name(&mut self) -> Result<String, String> {
        match self.advance() {
            Token::Name(n) => Ok(n),
            Token::Default => Err("DEFAULT is a reserved graph type name".into()),
            t => Err(format!("expected graph type name, got {t:?}")),
        }
    }

    fn graph_type_name_or_default(&mut self) -> Result<(String, bool), String> {
        match self.advance() {
            Token::Name(n) => Ok((n, false)),
            Token::Default => Ok(("DEFAULT".to_string(), true)),
            t => Err(format!("expected graph type name, got {t:?}")),
        }
    }

    // ===== Type-element body =====
    //
    // `{ TypeElement (, TypeElement)* }` where each element is a node
    // descriptor `(:L { ... })` or an edge triple
    // `(:A) -[:E { ... }]-> (:B)` / `(:A) ~[:E]~ (:B)` / `(:A) <-[:E]- (:B)`.

    fn type_body(&mut self) -> Result<Vec<TypeElement>, String> {
        self.expect(&Token::LBrace)?;
        let mut elements = Vec::new();
        if self.eat(&Token::RBrace) {
            return Ok(elements);
        }
        elements.push(self.type_element()?);
        while self.eat(&Token::Comma) {
            elements.push(self.type_element()?);
        }
        self.expect(&Token::RBrace)?;
        Ok(elements)
    }

    fn type_element(&mut self) -> Result<TypeElement, String> {
        let n1 = self.type_node()?;
        if !self.is_type_edge_start() {
            return Ok(TypeElement::Node(VariableType::Node(n1)));
        }
        let edge = self.type_edge_with_endpoints(n1)?;
        Ok(TypeElement::Edge(edge))
    }

    fn is_type_edge_start(&self) -> bool {
        matches!(
            self.peek(),
            Token::DashLB | Token::TildeLB | Token::LtDashLB
        )
    }

    /// Parse `( (:label_pattern)? type_record? )`. The body is `Closed`
    /// when a record is given (per the plan: only listed props are
    /// allowed and required), otherwise `Open` (no record means anything
    /// goes).
    fn type_node(&mut self) -> Result<DescriptorType, String> {
        self.expect(&Token::LParen)?;
        // Optional `:Label` — if missing, label is Star.
        let label = if self.eat(&Token::Colon) {
            // `(:)` with no label → Star (wildcard).
            if matches!(self.peek(), Token::RParen | Token::LBrace) {
                LabelType::Star
            } else {
                self.label_pattern()?
            }
        } else {
            LabelType::Star
        };
        let props = if matches!(self.peek(), Token::LBrace) {
            self.type_record_closed()?
        } else {
            PropertyType::open_empty()
        };
        self.expect(&Token::RParen)?;
        Ok(DescriptorType::new(label, props))
    }

    /// Parse the right side of an edge type-element: the edge bracket and
    /// the second node. The first node is supplied by the caller.
    fn type_edge_with_endpoints(
        &mut self,
        left_desc: DescriptorType,
    ) -> Result<VariableType, String> {
        let (edge_desc, kind) = self.type_edge_bracket()?;
        let right_desc = self.type_node()?;

        let (left_node, right_node) = match kind {
            EdgeKind::Right | EdgeKind::Undirected => (
                VariableType::Node(left_desc),
                VariableType::Node(right_desc),
            ),
            EdgeKind::Left => {
                // Normalize to LeftEndpoint=src direction so the runtime
                // model stays directional with src on the left.
                (
                    VariableType::Node(right_desc),
                    VariableType::Node(left_desc),
                )
            }
        };

        Ok(match kind {
            EdgeKind::Right | EdgeKind::Left => VariableType::EdgeDirectional {
                desc: edge_desc,
                left: Box::new(left_node),
                right: Box::new(right_node),
            },
            EdgeKind::Undirected => VariableType::EdgeNonDirectional {
                desc: edge_desc,
                left: Box::new(left_node),
                right: Box::new(right_node),
            },
        })
    }

    fn type_edge_bracket(&mut self) -> Result<(DescriptorType, EdgeKind), String> {
        match self.peek() {
            Token::DashLB => {
                self.advance();
                let (desc, _) = self.type_edge_filler()?;
                if self.eat(&Token::RBDashGt) {
                    Ok((desc, EdgeKind::Right))
                } else if self.eat(&Token::RBDash) {
                    // `-[E]-` (any-direction in queries) isn't part of
                    // the schema body grammar; users write `~[E]~` for
                    // undirected. Reject explicitly.
                    Err("schema edge body must be `-[...]->` (right) or `~[...]~` (undirected); got `-[...]-`".into())
                } else {
                    Err(format!(
                        "expected `]->` or `]-` to close edge bracket, got {:?}",
                        self.peek()
                    ))
                }
            }
            Token::TildeLB => {
                self.advance();
                let (desc, _) = self.type_edge_filler()?;
                self.expect(&Token::RBTilde)?;
                Ok((desc, EdgeKind::Undirected))
            }
            Token::LtDashLB => {
                self.advance();
                let (desc, _) = self.type_edge_filler()?;
                self.expect(&Token::RBDash)?;
                Ok((desc, EdgeKind::Left))
            }
            t => Err(format!("expected edge opening, got {t:?}")),
        }
    }

    /// Inside `[ : Label record? ]` for the schema body. Variables and
    /// WHERE clauses are not allowed here; reject them rather than
    /// silently dropping bindings.
    fn type_edge_filler(&mut self) -> Result<(DescriptorType, ()), String> {
        let label = if self.eat(&Token::Colon) {
            self.label_pattern()?
        } else {
            LabelType::Star
        };
        let props = if matches!(self.peek(), Token::LBrace) {
            self.type_record_closed()?
        } else {
            PropertyType::open_empty()
        };
        Ok((DescriptorType::new(label, props), ()))
    }

    /// Closed record body: `{ name STRING, age INT }`. Empty `{}` produces
    /// `Closed({})` — no extra properties allowed at all.
    fn type_record_closed(&mut self) -> Result<PropertyType, String> {
        self.expect(&Token::LBrace)?;
        let mut m = std::collections::BTreeMap::new();
        if self.eat(&Token::RBrace) {
            return Ok(PropertyType::Closed(m));
        }
        loop {
            let name = match self.advance() {
                Token::Name(n) => n,
                t => return Err(format!("expected property name, got {t:?}")),
            };
            let ty = self.schema_simple_type()?;
            m.insert(name, ty);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(PropertyType::Closed(m))
    }

    /// Schema-side simple type. Like `simple_type()` but additionally
    /// supports unions (`T | U`), the `LIST<T>` keyword form, and
    /// recursive records with optional `::` separators.
    fn schema_simple_type(&mut self) -> Result<SimpleType, String> {
        let mut left = self.schema_simple_type_atom()?;
        while self.eat(&Token::Pipe) {
            let right = self.schema_simple_type_atom()?;
            left = SimpleType::Union(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn schema_simple_type_atom(&mut self) -> Result<SimpleType, String> {
        match self.advance() {
            Token::Int => Ok(SimpleType::Z),
            Token::Float => Ok(SimpleType::F),
            Token::Bool => Ok(SimpleType::B),
            Token::Str => Ok(SimpleType::S),
            Token::Any | Token::Star => Ok(SimpleType::Star),
            Token::LBracket => {
                let inner = self.schema_simple_type()?;
                self.expect(&Token::RBracket)?;
                Ok(SimpleType::List(Box::new(inner)))
            }
            Token::List => {
                self.expect(&Token::Lt)?;
                let inner = self.schema_simple_type()?;
                self.expect(&Token::Gt)?;
                Ok(SimpleType::List(Box::new(inner)))
            }
            Token::LBrace => {
                let mut fields = std::collections::BTreeMap::new();
                if self.eat(&Token::RBrace) {
                    return Ok(SimpleType::Record(fields));
                }
                loop {
                    let k = match self.advance() {
                        Token::Name(n) => n,
                        t => return Err(format!("expected field name, got {t:?}")),
                    };
                    if matches!(self.peek(), Token::DoubleColon | Token::Typed) {
                        self.advance();
                    }
                    fields.insert(k, self.schema_simple_type()?);
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                self.expect(&Token::RBrace)?;
                Ok(SimpleType::Record(fields))
            }
            t => Err(format!(
                "expected schema type (INT/FLOAT/BOOL/STRING/ANY/LIST<T>/[T]/{{...}}), got {t:?}"
            )),
        }
    }
}

/// Recognize a §20.16 path function name (case-insensitive) and return
/// its canonical upper-case form for `Expr::Call`. `ELEMENTS` /
/// `PATH_LENGTH` / `CARDINALITY` are ISO; `NODES` / `EDGES` are the
/// non-standard translation helpers (documented as a divergence). Returns
/// `None` for any other name so it stays a plain variable reference.
fn path_function_name(name: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "ELEMENTS" | "NODES" | "EDGES" | "PATH_LENGTH" | "CARDINALITY"
    )
    .then_some(upper)
}

/// Direction of an edge in the schema body. Distinct from the runtime
/// `EdgeDir` because that one models the query side and includes `Any`,
/// which the schema grammar deliberately excludes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeKind {
    Right,
    Left,
    Undirected,
}
