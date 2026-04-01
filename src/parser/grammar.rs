use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr, UnOp};
use crate::syntax::path_pattern::PathPattern;
use crate::typing::descriptor_type::DescriptorType;
use crate::typing::label_type::LabelType;
use crate::typing::property_type::PropertyType;
use crate::typing::simple_type::SimpleType;

use super::lexer::{Lexer, Token};

/// Parse a GQL query string into a PathPattern.
pub fn parse(input: &str) -> Result<PathPattern, String> {
    let tokens = Lexer::tokenize(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let result = p.path_pattern()?;
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
        let mut where_expr: Option<Expr> = None;

        // Optional variable
        if let Token::Name(name) = self.peek().clone() {
            // Check it's not followed by something that makes this a subpattern
            let saved = self.pos;
            self.advance();
            // If next is ".", this is an attribute lookup, not a variable binding.
            // If next is another path-start token, this might be a subpattern variable.
            // For node patterns, after variable we expect : or WHERE or )
            match self.peek() {
                Token::Colon | Token::Where | Token::RParen => {
                    var = Some(name);
                }
                _ => {
                    // Not a valid element_pattern_filler
                    self.pos = saved;
                    return Err("not an element pattern filler".into());
                }
            }
        }

        // Optional ": type_schema"
        if self.eat(&Token::Colon) {
            dtype = Some(self.type_schema()?);
        }

        // Optional WHERE expr
        if self.eat(&Token::Where) {
            where_expr = Some(self.expr()?);
        }

        let dt = dtype.unwrap_or_else(DescriptorType::star);
        Ok((Descriptor::new(var, dt), where_expr))
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
        let mut where_expr: Option<Expr> = None;

        // Optional variable
        if let Token::Name(name) = self.peek().clone() {
            self.advance();
            var = Some(name);
        }

        // Optional ": type_schema"
        if self.eat(&Token::Colon) {
            dtype = Some(self.type_schema()?);
        }

        // Optional WHERE expr
        if self.eat(&Token::Where) {
            where_expr = Some(self.expr()?);
        }

        let dt = dtype.unwrap_or_else(DescriptorType::star);
        Ok((Descriptor::new(var, dt), where_expr))
    }

    // ===== Type schema =====

    // type_schema = label_pattern record_type | label_pattern | record_type
    fn type_schema(&mut self) -> Result<DescriptorType, String> {
        // Could start with label or record type
        match self.peek() {
            Token::LBrace | Token::DLBrace => {
                // Record type only (no label)
                let rt = self.record_type()?;
                Ok(DescriptorType::new(LabelType::Star, rt))
            }
            _ => {
                // Label pattern, optionally followed by record type
                let label = self.label_pattern()?;
                if matches!(self.peek(), Token::LBrace | Token::DLBrace) {
                    let rt = self.record_type()?;
                    Ok(DescriptorType::new(label, rt))
                } else {
                    Ok(DescriptorType::new(label, PropertyType::open_empty()))
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

    // record_type = open_record_type | closed_record_type
    fn record_type(&mut self) -> Result<PropertyType, String> {
        if self.check(&Token::DLBrace) {
            self.advance();
            if self.eat(&Token::DRBrace) {
                return Ok(PropertyType::closed_empty());
            }
            let mut pt = PropertyType::closed_empty();
            self.parse_record_elements(&mut pt)?;
            self.expect(&Token::DRBrace)?;
            Ok(pt)
        } else {
            self.expect(&Token::LBrace)?;
            if self.eat(&Token::RBrace) {
                return Ok(PropertyType::open_empty());
            }
            let mut pt = PropertyType::open_empty();
            self.parse_record_elements(&mut pt)?;
            self.expect(&Token::RBrace)?;
            Ok(pt)
        }
    }

    fn parse_record_elements(&mut self, pt: &mut PropertyType) -> Result<(), String> {
        let (name, ty) = self.record_element()?;
        pt.extend(name, ty);
        while self.eat(&Token::Comma) {
            let (name, ty) = self.record_element()?;
            pt.extend(name, ty);
        }
        Ok(())
    }

    fn record_element(&mut self) -> Result<(String, SimpleType), String> {
        let name = match self.advance() {
            Token::Name(n) => n,
            t => return Err(format!("expected attribute name, got {t:?}")),
        };
        self.expect(&Token::Colon)?;
        let ty = self.simple_type()?;
        Ok((name, ty))
    }

    fn simple_type(&mut self) -> Result<SimpleType, String> {
        match self.advance() {
            Token::Int => Ok(SimpleType::Z),
            Token::Bool => Ok(SimpleType::B),
            Token::Str => Ok(SimpleType::S),
            Token::Star => Ok(SimpleType::Star),
            t => Err(format!("expected type (int/bool/str/*), got {t:?}")),
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

    // primary = constant | attr_lookup | simple_type | "(" expr ")"
    fn primary_expr(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Number(n) => {
                self.advance();
                Ok(Expr::Const(Value::Int(n)))
            }
            Token::StringLit(s) => {
                self.advance();
                Ok(Expr::Const(Value::Str(s)))
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
                // Could be attribute lookup: name.name
                if self.eat(&Token::Dot) {
                    let attr = match self.advance() {
                        Token::Name(a) => a,
                        t => return Err(format!("expected attribute name after '.', got {t:?}")),
                    };
                    Ok(Expr::AttrLookup { var: name, attr })
                } else {
                    // Bare variable — treat as a lookup? This shouldn't normally happen
                    // in well-formed GQL but return it as a special case
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
