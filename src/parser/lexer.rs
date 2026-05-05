/// Token types for the GQL lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    Name(String),
    Number(i64),
    FloatLit(f64),
    StringLit(String),

    // Keywords
    True,
    False,
    Null,
    Where,
    And,
    Or,
    Not,
    As,
    In,
    Int,
    Float,
    Bool,
    Str,
    Match,
    Optional,
    Exists,
    Return,
    Distinct,
    All,
    /// ISO-39075 §16.x — `LIMIT <integer>` after RETURN. Produces
    /// `Query.limit = Some(N)`; the runtime caps row emission to N.
    Limit,
    /// ISO-39075 §14.9 + §16.16-17 — `ORDER BY <sort spec list>`.
    /// Compound token (only emitted when `ORDER` is followed by `BY`)
    /// so `order` and `by` stay valid as property names.
    OrderBy,
    /// `<ordering specification>` (§16.17). Both short and long forms;
    /// case-insensitive, reserved. `ASC`/`DESC` are short enough to be
    /// rare property names and the standard requires both.
    Asc,
    Desc,
    Ascending,
    Descending,
    /// `<null ordering>` (§16.17, Feature GA03). Compound to keep `nulls`
    /// and `first`/`last` usable as identifiers outside this context.
    NullsFirst,
    NullsLast,
    Typed,
    /// DDL / catalog keywords (uppercase only — see lexer rules).
    Create,
    Use,
    Drop,
    Graph,
    TypeKw,
    Default,
    /// Catalog inspection / validation keywords (uppercase only).
    Show,
    Current,
    Types,
    Validate,
    /// Secondary-index DDL (uppercase only).
    Index,
    Indexes,
    On,
    Using,
    Hash,
    BTree,
    /// `LIST` keyword for `LIST<T>` type-expression form.
    List,
    /// Compound: `group` and `by` stay valid as record/property names.
    GroupBy,
    Count,
    Sum,
    Avg,
    Min,
    Max,

    // Symbols
    LParen,      // (
    RParen,      // )
    LBracket,    // [
    RBracket,    // ]
    LBrace,      // {
    RBrace,      // }
    Colon,       // :
    DoubleColon, // ::
    Comma,       // ,
    Dot,         // .
    Star,        // *
    Plus,        // +
    Minus,       // -
    Bang,        // !
    Eq,          // =
    Ne,          // !=
    Lt,          // <
    Gt,          // >
    Le,          // <=
    Ge,          // >=
    Tilde,       // ~
    Question,    // ?
    Pipe,        // |
    Amp,         // &
    Semicolon,   // ;

    // Compound edge tokens
    RightArrow, // ->
    LeftArrow,  // <-
    DashLB,     // -[
    RBDashGt,   // ]->
    RBDash,     // ]-
    LtDashLB,   // <-[
    TildeLB,    // ~[
    RBTilde,    // ]~

    Eof,
}

pub struct Lexer {
    input: Vec<char>,
    pos: usize,
    pub tokens: Vec<Token>,
}

impl Lexer {
    pub fn tokenize(input: &str) -> Result<Vec<Token>, String> {
        let mut lexer = Lexer {
            input: input.chars().collect(),
            pos: 0,
            tokens: Vec::new(),
        };
        lexer.run()?;
        lexer.tokens.push(Token::Eof);
        Ok(lexer.tokens)
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.input.get(self.pos).copied();
        self.pos += 1;
        c
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    /// Used for aggregate soft-keyword detection: keyword only if next
    /// non-space char is `(`.
    fn peek_non_space(&self) -> Option<char> {
        let mut i = self.pos;
        while let Some(&c) = self.input.get(i) {
            if c.is_whitespace() {
                i += 1;
            } else {
                return Some(c);
            }
        }
        None
    }

    /// Used to detect compound keywords like `GROUP BY`: returns the
    /// next identifier (lowercased) and the position just past it.
    fn peek_next_word(&self) -> Option<(String, usize)> {
        let mut i = self.pos;
        while let Some(&c) = self.input.get(i) {
            if c.is_whitespace() {
                i += 1;
            } else {
                break;
            }
        }
        let start = i;
        while let Some(&c) = self.input.get(i) {
            if c.is_ascii_alphanumeric() || c == '_' {
                i += 1;
            } else {
                break;
            }
        }
        if i == start {
            return None;
        }
        let word: String = self.input[start..i]
            .iter()
            .collect::<String>()
            .to_lowercase();
        Some((word, i))
    }

    fn run(&mut self) -> Result<(), String> {
        loop {
            self.skip_whitespace();
            let Some(c) = self.peek() else {
                break;
            };

            match c {
                '(' => {
                    self.advance();
                    self.tokens.push(Token::LParen);
                }
                ')' => {
                    self.advance();
                    self.tokens.push(Token::RParen);
                }
                '[' => {
                    self.advance();
                    self.tokens.push(Token::LBracket);
                }
                ']' => {
                    self.advance();
                    match self.peek() {
                        Some('-') => {
                            self.advance();
                            if self.peek() == Some('>') {
                                self.advance();
                                self.tokens.push(Token::RBDashGt); // ]->
                            } else {
                                self.tokens.push(Token::RBDash); // ]-
                            }
                        }
                        Some('~') => {
                            self.advance();
                            self.tokens.push(Token::RBTilde); // ]~
                        }
                        _ => self.tokens.push(Token::RBracket),
                    }
                }
                '{' => {
                    self.advance();
                    self.tokens.push(Token::LBrace);
                }
                '}' => {
                    self.advance();
                    self.tokens.push(Token::RBrace);
                }
                ':' => {
                    self.advance();
                    if self.peek() == Some(':') {
                        self.advance();
                        self.tokens.push(Token::DoubleColon);
                    } else {
                        self.tokens.push(Token::Colon);
                    }
                }
                ',' => {
                    self.advance();
                    self.tokens.push(Token::Comma);
                }
                ';' => {
                    self.advance();
                    self.tokens.push(Token::Semicolon);
                }
                '.' => {
                    self.advance();
                    self.tokens.push(Token::Dot);
                }
                '*' => {
                    self.advance();
                    self.tokens.push(Token::Star);
                }
                '+' => {
                    self.advance();
                    self.tokens.push(Token::Plus);
                }
                '?' => {
                    self.advance();
                    self.tokens.push(Token::Question);
                }
                '|' => {
                    self.advance();
                    self.tokens.push(Token::Pipe);
                }
                '&' => {
                    self.advance();
                    self.tokens.push(Token::Amp);
                }
                '!' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.tokens.push(Token::Ne);
                    } else {
                        self.tokens.push(Token::Bang);
                    }
                }
                '=' => {
                    self.advance();
                    self.tokens.push(Token::Eq);
                }
                '~' => {
                    self.advance();
                    if self.peek() == Some('[') {
                        self.advance();
                        self.tokens.push(Token::TildeLB); // ~[
                    } else {
                        self.tokens.push(Token::Tilde);
                    }
                }
                '-' => {
                    self.advance();
                    match self.peek() {
                        Some('>') => {
                            self.advance();
                            self.tokens.push(Token::RightArrow); // ->
                        }
                        Some('[') => {
                            self.advance();
                            self.tokens.push(Token::DashLB); // -[
                        }
                        _ => self.tokens.push(Token::Minus),
                    }
                }
                '<' => {
                    self.advance();
                    match self.peek() {
                        Some('-') => {
                            self.advance();
                            if self.peek() == Some('[') {
                                self.advance();
                                self.tokens.push(Token::LtDashLB); // <-[
                            } else {
                                self.tokens.push(Token::LeftArrow); // <-
                            }
                        }
                        Some('=') => {
                            self.advance();
                            self.tokens.push(Token::Le);
                        }
                        _ => self.tokens.push(Token::Lt),
                    }
                }
                '>' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.tokens.push(Token::Ge);
                    } else {
                        self.tokens.push(Token::Gt);
                    }
                }
                '\'' => {
                    self.advance();
                    let mut s = String::new();
                    while let Some(ch) = self.peek() {
                        if ch == '\'' {
                            self.advance();
                            break;
                        }
                        s.push(ch);
                        self.advance();
                    }
                    self.tokens.push(Token::StringLit(s));
                }
                _ if c.is_ascii_digit() => {
                    let mut n = String::new();
                    while let Some(ch) = self.peek() {
                        if ch.is_ascii_digit() {
                            n.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    // Float fraction: only consume `.` if followed by a digit
                    // (so `x.foo` still tokenizes as Name, Dot, Name).
                    let mut is_float = false;
                    if self.peek() == Some('.') {
                        let next = self.input.get(self.pos + 1).copied();
                        if matches!(next, Some(ch) if ch.is_ascii_digit()) {
                            is_float = true;
                            n.push('.');
                            self.advance();
                            while let Some(ch) = self.peek() {
                                if ch.is_ascii_digit() {
                                    n.push(ch);
                                    self.advance();
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                    // Exponent
                    if matches!(self.peek(), Some('e') | Some('E')) {
                        is_float = true;
                        n.push(self.peek().unwrap());
                        self.advance();
                        if matches!(self.peek(), Some('+') | Some('-')) {
                            n.push(self.peek().unwrap());
                            self.advance();
                        }
                        while let Some(ch) = self.peek() {
                            if ch.is_ascii_digit() {
                                n.push(ch);
                                self.advance();
                            } else {
                                break;
                            }
                        }
                    }
                    if is_float {
                        self.tokens.push(Token::FloatLit(n.parse().unwrap()));
                    } else {
                        self.tokens.push(Token::Number(n.parse().unwrap()));
                    }
                }
                _ if c.is_ascii_alphabetic() || c == '_' => {
                    let mut name = String::new();
                    while let Some(ch) = self.peek() {
                        if ch.is_ascii_alphanumeric() || ch == '_' {
                            name.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let tok = match name.as_str() {
                        "true" => Token::True,
                        "false" => Token::False,
                        "null" | "NULL" => Token::Null,
                        "where" | "WHERE" => Token::Where,
                        "and" | "AND" => Token::And,
                        "or" | "OR" => Token::Or,
                        "not" | "NOT" => Token::Not,
                        "as" | "AS" => Token::As,
                        // `IS` is the ISO-39075 spelling of the type
                        // predicate; `TYPED` is the legacy alias kept for
                        // backward compatibility. Both produce the same
                        // token so `IS NULL` / `IS NOT NULL` and the
                        // typed-of operator share the same lookahead path.
                        "typed" | "TYPED" | "is" | "IS" => Token::Typed,
                        "in" | "IN" => Token::In,
                        "MATCH" | "match" => Token::Match,
                        "OPTIONAL" | "optional" => Token::Optional,
                        "EXISTS" | "exists" => Token::Exists,
                        "RETURN" | "return" => Token::Return,
                        "DISTINCT" | "distinct" => Token::Distinct,
                        "ALL" | "all" => Token::All,
                        "LIMIT" | "limit" => Token::Limit,
                        "GROUP" | "group"
                            if matches!(
                                self.peek_next_word().as_ref().map(|(w, _)| w.as_str()),
                                Some("by")
                            ) =>
                        {
                            if let Some((_, end)) = self.peek_next_word() {
                                self.pos = end;
                            }
                            Token::GroupBy
                        }
                        // `ORDER BY` is the only spelling we recognize
                        // ('ORDER' alone has no meaning in GQL grammar).
                        "ORDER" | "order"
                            if matches!(
                                self.peek_next_word().as_ref().map(|(w, _)| w.as_str()),
                                Some("by")
                            ) =>
                        {
                            if let Some((_, end)) = self.peek_next_word() {
                                self.pos = end;
                            }
                            Token::OrderBy
                        }
                        // `NULLS FIRST` / `NULLS LAST` recognized as compounds
                        // (Feature GA03). `nulls` alone stays available as a
                        // property name; the lookahead only fires when the
                        // next word is FIRST or LAST.
                        "NULLS" | "nulls"
                            if matches!(
                                self.peek_next_word().as_ref().map(|(w, _)| w.as_str()),
                                Some("first") | Some("last")
                            ) =>
                        {
                            let (next_word, end) = self.peek_next_word().unwrap();
                            self.pos = end;
                            if next_word == "first" {
                                Token::NullsFirst
                            } else {
                                Token::NullsLast
                            }
                        }
                        // ISO §16.17 <ordering specification>. Both short
                        // and long forms; case-insensitive.
                        "ASC" | "asc" => Token::Asc,
                        "DESC" | "desc" => Token::Desc,
                        "ASCENDING" | "ascending" => Token::Ascending,
                        "DESCENDING" | "descending" => Token::Descending,
                        // Soft keywords: only when followed by `(`. Keeps
                        // `{count: 1}`, `x.sum`, etc. working as identifiers.
                        "COUNT" | "count" if self.peek_non_space() == Some('(') => Token::Count,
                        "SUM" | "sum" if self.peek_non_space() == Some('(') => Token::Sum,
                        "AVG" | "avg" if self.peek_non_space() == Some('(') => Token::Avg,
                        "MIN" | "min" if self.peek_non_space() == Some('(') => Token::Min,
                        "MAX" | "max" if self.peek_non_space() == Some('(') => Token::Max,
                        "int" => Token::Int,
                        "float" => Token::Float,
                        "bool" => Token::Bool,
                        "str" => Token::Str,
                        // ISO-style uppercase primitive aliases. Only
                        // recognized in their canonical uppercase form to
                        // keep lowercase identifiers (`string`, `integer`,
                        // `boolean`, `list`, `any`) usable as property
                        // names in existing graphs and queries.
                        "INT" | "INTEGER" => Token::Int,
                        "FLOAT" => Token::Float,
                        "BOOL" | "BOOLEAN" => Token::Bool,
                        "STRING" => Token::Str,
                        "ANY" => Token::Star,
                        "LIST" => Token::List,
                        // DDL keywords. Uppercase-only for the same
                        // backward-compat reason: `type` and `default` are
                        // common property names.
                        "CREATE" => Token::Create,
                        "USE" => Token::Use,
                        "DROP" => Token::Drop,
                        "GRAPH" => Token::Graph,
                        "TYPE" => Token::TypeKw,
                        "TYPES" => Token::Types,
                        "DEFAULT" => Token::Default,
                        "SHOW" => Token::Show,
                        "CURRENT" => Token::Current,
                        "VALIDATE" => Token::Validate,
                        "INDEX" => Token::Index,
                        "INDEXES" => Token::Indexes,
                        "ON" => Token::On,
                        "USING" => Token::Using,
                        "HASH" => Token::Hash,
                        "BTREE" => Token::BTree,
                        _ => Token::Name(name),
                    };
                    self.tokens.push(tok);
                }
                _ => {
                    return Err(format!(
                        "unexpected character: '{c}' at position {}",
                        self.pos
                    ));
                }
            }
        }
        Ok(())
    }
}
