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
    As,
    In,
    Int,
    Float,
    Bool,
    Str,
    Match,
    Return,
    Distinct,
    Typed,

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
                        "as" | "AS" => Token::As,
                        "typed" | "TYPED" => Token::Typed,
                        "in" | "IN" => Token::In,
                        "MATCH" | "match" => Token::Match,
                        "RETURN" | "return" => Token::Return,
                        "DISTINCT" | "distinct" => Token::Distinct,
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
