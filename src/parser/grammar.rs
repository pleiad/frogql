use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{Query, ReturnItem};
use crate::typing::descriptor_type::DescriptorType;
use crate::typing::label_type::LabelType;
use crate::typing::property_type::PropertyType;
use crate::typing::simple_type::SimpleType;

use super::lexer::{Lexer, Token};

/// Parse a GQL path pattern string into a PathPattern (backwards compatible).
pub fn parse(input: &str) -> Result<PathPattern, String> {
    let q = parse_query(input)?;
    Ok(q.pattern)
}

/// Parse a full GQL query: optional MATCH, path pattern, optional WHERE, optional RETURN.
pub fn parse_query(input: &str) -> Result<Query, String> {
    let tokens = Lexer::tokenize(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let result = p.full_query()?;
    if !p.at_eof() {
        return Err(format!("unexpected token {:?} at position {}", p.peek(), p.pos));
    }
    Ok(result)
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

    fn eat(&mut self, tok: &Token) -> bool {
        if self.check(tok) {
            self.advance();
            true
        } else {
            false
        }
    }

    // ===== Full query (MATCH ... WHERE ... RETURN) =====

    // full_query = ("MATCH")? query ("WHERE" expr)? ("RETURN" ("DISTINCT")? return_list)?
    fn full_query(&mut self) -> Result<Query, String> {
        // Optional MATCH keyword
        self.eat(&Token::Match);

        // Parse the pattern (path_pattern with comma-joins)
        let mut pattern = self.query()?;

        // Optional WHERE clause (wraps pattern in Filter)
        if self.eat(&Token::Where) {
            let expr = self.expr()?;
            pattern = PathPattern::Filter(Box::new(pattern), expr);
        }

        // Optional RETURN clause
        if self.eat(&Token::Return) {
            let distinct = self.eat(&Token::Distinct);
            let returns = self.return_list()?;
            Ok(Query { pattern, returns: Some(returns), distinct })
        } else {
            Ok(Query::pattern_only(pattern))
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

    // return_item = expr ("AS" NAME)?
    // Tricky: AS is also a type-cast operator in expressions (x.val as int).
    // We disambiguate: if AS is followed by a Name (not a type keyword), it's an alias.
    fn return_item(&mut self) -> Result<ReturnItem, String> {
        let expr = self.return_expr()?;
        // Check for "AS <name>" alias
        if self.check(&Token::As) {
            let saved = self.pos;
            self.advance(); // consume AS
            if let Token::Name(n) = self.peek().clone() {
                self.advance();
                return Ok(ReturnItem { expr, alias: Some(n) });
            }
            // Not a name after AS — backtrack, it wasn't an alias
            self.pos = saved;
        }
        Ok(ReturnItem { expr, alias: None })
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
            left = Expr::Binop { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    // Like comparison() but without AS (so it's available for alias).
    fn return_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        loop {
            let op = match self.peek() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::Le => BinOp::Le,
                Token::Ge => BinOp::Ge,
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Is => BinOp::Is,
                // No Token::As here — reserved for alias
                _ => break,
            };
            self.advance();
            let right = self.term()?;
            left = Expr::Binop { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    // ===== Queries (comma-join level) =====

    // query = path_pattern ("," path_pattern)*
    fn query(&mut self) -> Result<PathPattern, String> {
        let mut left = self.path_pattern()?;
        while self.eat(&Token::Comma) {
            let right = self.path_pattern()?;
            left = PathPattern::Join(Box::new(left), Box::new(right));
        }
        Ok(left)
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

    // Try to parse element_pattern_filler: variable? (":" type_schema)? (WHERE expr)?
    // Returns Ok((Descriptor, Option<Expr>)) on success.
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
                Token::Colon | Token::Where | Token::RParen => {
                    var = Some(name);
                }
                _ => {
                    self.pos = saved;
                    return Err("not an element pattern filler".into());
                }
            }
        }

        // Optional ": type_schema"
        if self.eat(&Token::Colon) {
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

        if self.eat(&Token::Colon) {
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
                    Ok((DescriptorType::new(label, PropertyType::open_empty()), Vec::new()))
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
            Token::Star => {
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
        let mut pt = if is_closed { PropertyType::closed_empty() } else { PropertyType::open_empty() };
        let mut filters = Vec::new();
        if self.eat(&Token::RBrace) {
            if is_closed { self.expect(&Token::RBrace)?; }
            return Ok((pt, filters));
        }
        self.parse_record_elements(&mut pt, &mut filters)?;
        self.expect(&Token::RBrace)?;
        if is_closed { self.expect(&Token::RBrace)?; }
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

    /// Parse one record element, either:
    ///   `name is T`  — type ascription (new canonical form)
    ///   `name : T`   — type ascription (legacy; only when next token is a type keyword)
    ///   `name : e`   — value-equality filter (elaborated to `name = e` in WHERE)
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
            Token::Is => {
                self.advance();
                let ty = self.simple_type()?;
                pt.extend(name, ty);
            }
            Token::Colon => {
                self.advance();
                // Disambiguate by peeking: a lone type keyword followed by `,` or the
                // closing brace is a type ascription (legacy); anything else is a value.
                let is_type_head = matches!(
                    self.peek(),
                    Token::Int | Token::Float | Token::Bool | Token::Str | Token::Star
                );
                let followed_by_terminator = matches!(
                    self.peek_at(1),
                    Some(Token::Comma) | Some(Token::RBrace)
                );
                if is_type_head && followed_by_terminator {
                    let ty = self.simple_type()?;
                    pt.extend(name, ty);
                } else {
                    let e = self.expr()?;
                    filters.push((name, e));
                }
            }
            t => return Err(format!("expected 'is' or ':' after record key, got {t:?}")),
        }
        Ok(())
    }

    fn simple_type(&mut self) -> Result<SimpleType, String> {
        match self.advance() {
            Token::Int => Ok(SimpleType::Z),
            Token::Float => Ok(SimpleType::F),
            Token::Bool => Ok(SimpleType::B),
            Token::Str => Ok(SimpleType::S),
            Token::Star => Ok(SimpleType::Star),
            Token::LBracket => {
                let inner = self.simple_type()?;
                self.expect(&Token::RBracket)?;
                Ok(SimpleType::List(Box::new(inner)))
            }
            Token::LBrace => {
                // Record type `{k: T, k2: T2, ...}`. The `:` separator follows JSON
                // and the rest of the type language; ambiguity with record value
                // literals only arises in expression position and is resolved there
                // via speculative parsing.
                let mut fields = std::collections::BTreeMap::new();
                if self.eat(&Token::RBrace) {
                    return Ok(SimpleType::Record(fields));
                }
                loop {
                    let k = match self.advance() {
                        Token::Name(n) => n,
                        t => return Err(format!("expected field name, got {t:?}")),
                    };
                    self.expect(&Token::Colon)?;
                    fields.insert(k, self.simple_type()?);
                    if !self.eat(&Token::Comma) { break; }
                }
                self.expect(&Token::RBrace)?;
                Ok(SimpleType::Record(fields))
            }
            t => Err(format!("expected type (int/float/bool/str/*/[T]/{{k: T}}), got {t:?}")),
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
            let op = match self.peek() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::Le => BinOp::Le,
                Token::Ge => BinOp::Ge,
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Is => BinOp::Is,
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

    // term = unary ("+" unary)*
    fn term(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while self.eat(&Token::Plus) {
            let right = self.unary()?;
            left = Expr::Binop {
                op: BinOp::Add,
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
            let operand = self.unary()?;
            return Ok(Expr::Unop {
                op: UnOp::Not,
                operand: Box::new(operand),
            });
        }
        self.primary_expr()
    }

    // primary = constant | list_literal | attr_lookup | simple_type | "(" expr ")"
    fn primary_expr(&mut self) -> Result<Expr, String> {
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
                    if let Expr::Const(v) = e { consts.push(v.clone()); }
                    else { all_const = false; break; }
                }
                if all_const {
                    Ok(Expr::Const(Value::List(consts)))
                } else {
                    Err("non-constant list literal elements are not supported yet".into())
                }
            }
            Token::LBrace => {
                // Records use `{k: T, ...}` for types and `{k: v, ...}` for values —
                // same separator, different contexts. Here both syntaxes share a token
                // stream, so speculate on the type parse first, fall back to value.
                self.advance();
                if self.eat(&Token::RBrace) {
                    return Ok(Expr::Const(Value::Record(std::collections::BTreeMap::new())));
                }
                let saved = self.pos;
                // Try record type: each entry `name : simple_type`.
                let mut type_fields: std::collections::BTreeMap<String, SimpleType> =
                    std::collections::BTreeMap::new();
                let type_ok = (|| -> Result<bool, ()> {
                    loop {
                        let k = match self.advance() {
                            Token::Name(n) => n,
                            _ => return Err(()),
                        };
                        if !matches!(self.peek(), Token::Colon) { return Err(()); }
                        self.advance();
                        let ty = self.simple_type().map_err(|_| ())?;
                        type_fields.insert(k, ty);
                        if !self.eat(&Token::Comma) { break; }
                    }
                    if !self.eat(&Token::RBrace) { return Err(()); }
                    Ok(true)
                })();
                if type_ok.is_ok() {
                    return Ok(Expr::Type(SimpleType::Record(type_fields)));
                }
                // Fall back to record value: each entry `name : expr (constant)`.
                self.pos = saved;
                let mut value_fields: std::collections::BTreeMap<String, Value> =
                    std::collections::BTreeMap::new();
                loop {
                    let k = match self.advance() {
                        Token::Name(n) => n,
                        t => return Err(format!("expected field name, got {t:?}")),
                    };
                    self.expect(&Token::Colon)?;
                    match self.expr()? {
                        Expr::Const(val) => { value_fields.insert(k, val); }
                        _ => return Err("non-constant record literal values are not supported yet".into()),
                    }
                    if !self.eat(&Token::Comma) { break; }
                }
                self.expect(&Token::RBrace)?;
                Ok(Expr::Const(Value::Record(value_fields)))
            }
            Token::True => {
                self.advance();
                Ok(Expr::Const(Value::Bool(true)))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Const(Value::Bool(false)))
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
            Token::Star => {
                self.advance();
                Ok(Expr::Type(SimpleType::Star))
            }
            Token::Name(name) => {
                self.advance();
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
                        expr = Expr::FieldAccess { base: Box::new(expr), field };
                    }
                    Ok(expr)
                } else {
                    Err(format!("unexpected bare variable '{name}' in expression"))
                }
            }
            Token::LParen => {
                self.advance();
                let e = self.expr()?;
                self.expect(&Token::RParen)?;
                Ok(e)
            }
            _ => Err(format!("expected expression, got {:?}", self.peek())),
        }
    }
}
