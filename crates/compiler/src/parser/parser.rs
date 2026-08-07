//! AIKOQL Parser — recursive descent per MRFC-0010 §3.
//! Error codes and messages are defined in `diagnostics.rs`.

use super::ast::*;
use super::diagnostics::{self, Diagnostic};
use super::lexer::{Lexer, Token};

pub type ParseError = Diagnostic;

// Local helpers that wrap diagnostics constructors with token formatting.
fn unexpected(got: &Token, line: usize, col: usize) -> Diagnostic {
    diagnostics::unexpected_token(&format!("{:?}", got), line, col)
}
fn expected_err(desc: &str, got: &Token, line: usize, col: usize) -> Diagnostic {
    diagnostics::expected_token(desc, &format!("{:?}", got), line, col)
}
fn eof_err(line: usize, col: usize) -> Diagnostic {
    diagnostics::unexpected_eof(line, col)
}
fn invalid_op(got: &Token, line: usize, col: usize) -> Diagnostic {
    diagnostics::invalid_operator(&format!("{:?}", got), line, col)
}

// ---- Parser ----
pub struct Parser {
    lexer: Lexer,
    current: Token,
    line: usize,
    col: usize,
}

impl Parser {
    pub fn new(source: &str) -> Self {
        let mut lexer = Lexer::new(source);
        let current = lexer.next_token();
        Parser { lexer, current, line: 1, col: 1 }
    }
    fn advance(&mut self) { self.current = self.lexer.next_token(); }

    fn expect(&mut self, expected: Token) -> Result<(), ParseError> {
        if std::mem::discriminant(&self.current) == std::mem::discriminant(&expected) {
            self.advance(); Ok(())
        } else {
            Err(expected_err(&format!("{:?}", expected), &self.current, self.line, self.col))
        }
    }

    fn expect_ident(&mut self, desc: &str) -> Result<String, ParseError> {
        match &self.current {
            Token::Ident(s) => { let v = s.clone(); self.advance(); Ok(v) }
            _ => Err(expected_err(desc, &self.current, self.line, self.col)),
        }
    }
    fn expect_string(&mut self) -> Result<String, ParseError> {
        match &self.current {
            Token::StringLit(s) => { let v = s.clone(); self.advance(); Ok(v) }
            _ => Err(expected_err("string literal", &self.current, self.line, self.col)),
        }
    }

    pub fn parse_statement(&mut self) -> Result<Statement, ParseError> {
        match &self.current {
            Token::Match => self.parse_match().map(Statement::Match),
            Token::Create => self.parse_create().map(Statement::Create),
            Token::Update => self.parse_update().map(Statement::Update),
            Token::Delete => self.parse_delete().map(Statement::Delete),
            Token::Ingest => self.parse_ingest().map(Statement::Ingest),
            _ => Err(unexpected(&self.current, self.line, self.col)
                .with_hint("try MATCH, CREATE, UPDATE, DELETE, or INGEST")),
        }
    }

    fn parse_match(&mut self) -> Result<MatchStatement, ParseError> {
        self.expect(Token::Match)?;
        let entity = self.expect_ident("entity name")?;
        let mut preds = Vec::new(); let mut sim = None; let mut trav = None;
        loop {
            match &self.current {
                Token::Where | Token::And | Token::Or => { self.advance(); preds.push(self.parse_predicate()?); }
                Token::Similar => {
                    self.advance(); self.expect(Token::To)?;
                    sim = Some(SimilarityClause { query: self.expect_string()? });
                }
                Token::Traverse => {
                    self.advance();
                    trav = Some(TraverseClause { relation: self.expect_ident("relation name")? });
                }
                Token::Return => {
                    self.advance();
                    return Ok(MatchStatement { entity, predicates: preds, similarity: sim, traverse: trav, projection: self.parse_projection()? });
                }
                Token::Eof => return Err(eof_err(self.line, self.col).with_hint("add RETURN clause")),
                _ => return Err(unexpected(&self.current, self.line, self.col).with_hint("expected WHERE, SIMILAR, TRAVERSE, or RETURN")),
            }
        }
    }

    fn parse_predicate(&mut self) -> Result<Predicate, ParseError> {
        let prop = self.expect_ident("property name")?;
        let op = match &self.current {
            Token::Eq => "eq", Token::Neq => "neq", Token::Gt => "gt", Token::Lt => "lt",
            Token::Gte => "gte", Token::Lte => "lte",
            _ => return Err(invalid_op(&self.current, self.line, self.col)),
        };
        self.advance();
        let val = self.parse_expr()?;
        Ok(match op {
            "eq" => Predicate::Eq { property: prop, value: val },
            "neq" => Predicate::Neq { property: prop, value: val },
            "gt" => Predicate::Gt { property: prop, value: val },
            "lt" => Predicate::Lt { property: prop, value: val },
            "gte" => Predicate::Gte { property: prop, value: val },
            "lte" => Predicate::Lte { property: prop, value: val },
            _ => unreachable!(),
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        match &self.current {
            Token::StringLit(s) => { let v = s.clone(); self.advance(); Ok(Expr::String(v)) }
            Token::Number(n) => { let v = *n; self.advance(); Ok(Expr::Number(v)) }
            Token::Ident(s) if s == "true" => { self.advance(); Ok(Expr::Bool(true)) }
            Token::Ident(s) if s == "false" => { self.advance(); Ok(Expr::Bool(false)) }
            Token::Ident(s) if s == "null" => { self.advance(); Ok(Expr::Null) }
            _ => Err(expected_err("expression (string, number, bool, null)", &self.current, self.line, self.col)),
        }
    }

    fn parse_projection(&mut self) -> Result<Projection, ParseError> {
        match &self.current {
            Token::Star => { self.advance(); Ok(Projection::Star) }
            Token::Explain => { self.advance(); Ok(Projection::Explain) }
            Token::Ident(_) => {
                let mut fields = vec![self.expect_ident("field name")?];
                while let Token::Comma = &self.current { self.advance(); fields.push(self.expect_ident("field name")?); }
                Ok(Projection::Fields(fields))
            }
            _ => Err(expected_err("*, explain, or field names", &self.current, self.line, self.col)),
        }
    }

    fn parse_ingest(&mut self) -> Result<IngestStatement, ParseError> {
        self.expect(Token::Ingest)?;
        let source = self.expect_string()?;
        let (mut et, mut ee, mut br) = (false, false, false);
        loop {
            match &self.current {
                Token::Extract => { self.advance();
                    match &self.current {
                        Token::Tables => { et = true; self.advance(); }
                        Token::Entities => { ee = true; self.advance(); }
                        _ => return Err(expected_err("TABLES or ENTITIES", &self.current, self.line, self.col)),
                    }
                }
                Token::Build => { self.advance(); self.expect(Token::Relationships)?; br = true; }
                Token::Commit => { self.advance(); return Ok(IngestStatement { source, extract_tables: et, extract_entities: ee, build_relationships: br }); }
                _ => break,
            }
        }
        Ok(IngestStatement { source, extract_tables: et, extract_entities: ee, build_relationships: br })
    }

    fn parse_create(&mut self) -> Result<CreateStatement, ParseError> {
        self.expect(Token::Create)?;
        let entity = self.expect_ident("entity name")?;
        let mut props = Vec::new();
        if let Token::Ident(_) = &self.current {
            loop {
                let k = self.expect_ident("property name")?;
                self.expect(Token::Eq)?;
                props.push((k, self.parse_expr()?));
                if !matches!(&self.current, Token::Comma) { break; }
                self.advance();
            }
        }
        Ok(CreateStatement { entity, properties: props })
    }

    fn parse_update(&mut self) -> Result<UpdateStatement, ParseError> {
        self.expect(Token::Update)?;
        let entity = self.expect_ident("entity name")?;
        let koid = self.expect_string()?;
        let mut props = Vec::new();
        if let Token::Ident(_) = &self.current {
            loop {
                let k = self.expect_ident("property name")?;
                self.expect(Token::Eq)?;
                props.push((k, self.parse_expr()?));
                if !matches!(&self.current, Token::Comma) { break; }
                self.advance();
            }
        }
        Ok(UpdateStatement { entity, koid, properties: props })
    }

    fn parse_delete(&mut self) -> Result<DeleteStatement, ParseError> {
        self.expect(Token::Delete)?;
        Ok(DeleteStatement { entity: self.expect_ident("entity name")?, koid: self.expect_string()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn parse_simple_match() {
        let mut p = Parser::new("MATCH Person RETURN *");
        match p.parse_statement().unwrap() {
            Statement::Match(m) => { assert_eq!(m.entity, "Person"); assert_eq!(m.projection, Projection::Star); }
            _ => panic!(),
        }
    }
    #[test] fn parse_match_with_predicate() {
        match Parser::new("MATCH Person WHERE company == \"Visa\" RETURN *").parse_statement().unwrap() {
            Statement::Match(m) => assert_eq!(m.predicates.len(), 1),
            _ => panic!(),
        }
    }
    #[test] fn parse_match_similar_traverse() {
        match Parser::new("MATCH Person SIMILAR TO \"John\" TRAVERSE managed_by WHERE company == \"Visa\" RETURN explain").parse_statement().unwrap() {
            Statement::Match(m) => { assert!(m.similarity.is_some()); assert!(m.traverse.is_some()); }
            _ => panic!(),
        }
    }
    #[test] fn parse_update() {
        match Parser::new("UPDATE Person \"abc\" name == \"Bob\"").parse_statement().unwrap() {
            Statement::Update(u) => { assert_eq!(u.koid, "abc"); assert_eq!(u.properties.len(), 1); }
            _ => panic!(),
        }
    }
    #[test] fn parse_delete() {
        match Parser::new("DELETE Person \"abc\"").parse_statement().unwrap() {
            Statement::Delete(d) => assert_eq!(d.koid, "abc"),
            _ => panic!(),
        }
    }
    #[test] fn parse_create() {
        match Parser::new("CREATE Person name == \"Alice\", age == 30").parse_statement().unwrap() {
            Statement::Create(c) => { assert_eq!(c.entity, "Person"); assert_eq!(c.properties.len(), 2); }
            _ => panic!(),
        }
    }
    #[test] fn error_has_code_and_hint() {
        let e = Parser::new("BOGUS").parse_statement().unwrap_err();
        assert_eq!(e.code, diagnostics::Code::UnexpectedToken);
        assert!(e.format().contains("AIKOQL1010"));
        assert!(e.hint.is_some());
    }
    #[test] fn error_invalid_operator() {
        let e = Parser::new("MATCH Person WHERE name = \"x\" RETURN *").parse_statement().unwrap_err();
        assert_eq!(e.code, diagnostics::Code::InvalidOperator);
        assert!(e.hint.unwrap().contains("=="));
    }
}
