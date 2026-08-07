//! AIKOQL Lexer — tokenizer per MRFC-0010 §3 (Lexer).
//!
//! Hand-written, zero-allocation where possible. Produces a `Token` stream
//! with source spans for diagnostics.

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    // Keywords
    Match,
    Where,
    And,
    Or,
    Return,
    Similar,
    To,
    Traverse,
    Create,
    Update,
    Delete,
    Ingest,
    Extract,
    Tables,
    Entities,
    Build,
    Relationships,
    Commit,
    Explain,
    // Symbols
    Eq,       // ==
    Neq,      // !=
    Lt,       // <
    Gt,       // >
    Lte,      // <=
    Gte,      // >=
    LParen,   // (
    RParen,   // )
    Comma,    // ,
    Dot,      // .
    Star,     // *
    // Literals
    Ident(String),
    StringLit(String),
    Number(f64),
    // Special
    Eof,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

pub struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Lexer {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn span(&self) -> Span {
        Span {
            line: self.line,
            col: self.col,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        c
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.advance();
            } else if c == '/' && self.chars.get(self.pos + 1) == Some(&'/') {
                // Line comment: skip to end of line.
                while let Some(ch) = self.advance() {
                    if ch == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn read_word(&mut self, first: char) -> Token {
        let mut s = String::new();
        s.push(first);
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        match s.to_uppercase().as_str() {
            "MATCH" => Token::Match,
            "WHERE" => Token::Where,
            "AND" => Token::And,
            "OR" => Token::Or,
            "RETURN" => Token::Return,
            "SIMILAR" => Token::Similar,
            "TO" => Token::To,
            "TRAVERSE" => Token::Traverse,
            "CREATE" => Token::Create,
            "UPDATE" => Token::Update,
            "DELETE" => Token::Delete,
            "INGEST" => Token::Ingest,
            "EXTRACT" => Token::Extract,
            "TABLES" => Token::Tables,
            "ENTITIES" => Token::Entities,
            "BUILD" => Token::Build,
            "RELATIONSHIPS" => Token::Relationships,
            "COMMIT" => Token::Commit,
            "EXPLAIN" => Token::Explain,
            _ => Token::Ident(s),
        }
    }

    fn read_string(&mut self) -> Token {
        let mut s = String::new();
        while let Some(c) = self.advance() {
            if c == '"' {
                return Token::StringLit(s);
            }
            s.push(c);
        }
        Token::Error("unterminated string".into())
    }

    fn read_number(&mut self, first: char) -> Token {
        let mut s = String::new();
        s.push(first);
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        match s.parse::<f64>() {
            Ok(n) => Token::Number(n),
            Err(_) => Token::Error(format!("invalid number: {}", s)),
        }
    }

    pub fn next_token(&mut self) -> Token {
        self.skip_whitespace();
        let span = self.span();
        match self.advance() {
            None => Token::Eof,
            Some(c) if c.is_alphabetic() || c == '_' => self.read_word(c),
            Some(c) if c.is_ascii_digit() => self.read_number(c),
            Some('"') => self.read_string(),
            Some('=') => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::Eq
                } else {
                    Token::Error("expected '=='".into())
                }
            }
            Some('!') => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::Neq
                } else {
                    Token::Error("expected '!='".into())
                }
            }
            Some('<') => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::Lte
                } else {
                    Token::Lt
                }
            }
            Some('>') => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::Gte
                } else {
                    Token::Gt
                }
            }
            Some('(') => Token::LParen,
            Some(')') => Token::RParen,
            Some(',') => Token::Comma,
            Some('.') => Token::Dot,
            Some('*') => Token::Star,
            Some(other) => Token::Error(format!("unexpected character '{}' at {}:{}", other, span.line, span.col)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(source: &str) -> Vec<Token> {
        let mut lex = Lexer::new(source);
        let mut out = Vec::new();
        loop {
            let t = lex.next_token();
            let done = matches!(t, Token::Eof | Token::Error(_));
            out.push(t);
            if done {
                break;
            }
        }
        out
    }

    #[test]
    fn lex_match_where_return() {
        let ts = tokens("MATCH Person WHERE company == \"Visa\" RETURN *");
        assert_eq!(ts[0], Token::Match);
        assert_eq!(ts[1], Token::Ident("Person".into()));
        assert_eq!(ts[2], Token::Where);
        assert_eq!(ts[3], Token::Ident("company".into()));
        assert_eq!(ts[4], Token::Eq);
        assert_eq!(ts[5], Token::StringLit("Visa".into()));
        assert_eq!(ts[6], Token::Return);
        assert_eq!(ts[7], Token::Star);
    }

    #[test]
    fn lex_similar_traverse() {
        let ts = tokens("MATCH Person SIMILAR TO \"John\" TRAVERSE managed_by RETURN explain");
        assert_eq!(ts[0], Token::Match);
        assert_eq!(ts[1], Token::Ident("Person".into()));
        assert_eq!(ts[2], Token::Similar);
        assert_eq!(ts[3], Token::To);
        assert_eq!(ts[4], Token::StringLit("John".into()));
        assert_eq!(ts[5], Token::Traverse);
        assert_eq!(ts[6], Token::Ident("managed_by".into()));
        assert_eq!(ts[7], Token::Return);
        assert_eq!(ts[8], Token::Explain);
    }

    #[test]
    fn lex_line_comment() {
        let ts = tokens("// comment\nMATCH Person RETURN *");
        assert_eq!(ts[0], Token::Match);
        assert_eq!(ts[1], Token::Ident("Person".into()));
    }
}
