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
    Where,
    And,
    Or,
    Not,
    Is,
    As,
    In,
    Int,
    Float,
    Bool,
    Str,
    Match,
    Return,
    Distinct,
    All,
    /// `GROUP BY` — emitted as a single token to avoid making `group` and `by`
    /// each their own keyword (both are common as property/field names).
    GroupBy,
    // Aggregate function names (ISO §20.9, core kinds)
    Count,
    Sum,
    Avg,
    Min,
    Max,

    // Symbols
    LParen,   // (
    RParen,   // )
    LBracket, // [
    RBracket, // ]
    LBrace,   // {
    RBrace,   // }
    Colon,    // :
    Comma,    // ,
    Dot,      // .
    Star,     // *
    Plus,     // +
    Minus,    // -
    Bang,     // !
    Eq,       // =
    Ne,       // !=
    Lt,       // <
    Gt,       // >
    Le,       // <=
    Ge,       // >=
    Tilde,    // ~
    Question, // ?
    Pipe,     // |
    Amp,      // &

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

    /// Peek the next non-whitespace character without advancing the position.
    /// Used by aggregate-name soft-keyword detection: `count` is `Token::Count`
    /// only when followed (across whitespace) by `(`; otherwise it stays a
    /// regular `Name` so existing code using `count` as a property/field
    /// keeps working.
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

    /// Peek the next identifier-shaped word starting from the current
    /// position, skipping whitespace. Returns the lowercased word and the
    /// position just past its last char, or None if no identifier follows.
    /// Used to detect compound keywords like `GROUP BY` without making
    /// `group` and `by` each their own keyword.
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
                    self.tokens.push(Token::Colon);
                }
                ',' => {
                    self.advance();
                    self.tokens.push(Token::Comma);
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
                        "where" | "WHERE" => Token::Where,
                        "and" | "AND" => Token::And,
                        "or" | "OR" => Token::Or,
                        "not" | "NOT" => Token::Not,
                        "is" | "IS" => Token::Is,
                        "as" | "AS" => Token::As,
                        "in" | "IN" => Token::In,
                        "MATCH" | "match" => Token::Match,
                        "RETURN" | "return" => Token::Return,
                        "DISTINCT" | "distinct" => Token::Distinct,
                        "ALL" | "all" => Token::All,
                        // GROUP BY is a compound keyword: only emitted as
                        // Token::GroupBy when `group` is followed by `by`
                        // across whitespace. Otherwise `group` stays a Name
                        // (so `{group: 1}` keeps working). Same logic
                        // protects bare `by`.
                        "GROUP" | "group"
                            if matches!(
                                self.peek_next_word().as_ref().map(|(w, _)| w.as_str()),
                                Some("by")
                            ) =>
                        {
                            // Consume the trailing `by` we just peeked.
                            if let Some((_, end)) = self.peek_next_word() {
                                self.pos = end;
                            }
                            Token::GroupBy
                        }
                        // Aggregate function names are SOFT keywords: only
                        // tokenized as the keyword when immediately followed
                        // (across whitespace) by `(`. This preserves backward
                        // compat with property/field names like `{count: 1}`.
                        "COUNT" | "count" if self.peek_non_space() == Some('(') => Token::Count,
                        "SUM" | "sum" if self.peek_non_space() == Some('(') => Token::Sum,
                        "AVG" | "avg" if self.peek_non_space() == Some('(') => Token::Avg,
                        "MIN" | "min" if self.peek_non_space() == Some('(') => Token::Min,
                        "MAX" | "max" if self.peek_non_space() == Some('(') => Token::Max,
                        "int" => Token::Int,
                        "float" => Token::Float,
                        "bool" => Token::Bool,
                        "str" => Token::Str,
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
