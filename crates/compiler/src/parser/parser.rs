//! Aikoql Parser — recursive descent per MRFC-0010 §3.
//! Error codes and messages are defined in `diagnostics.rs`.

use super::ast::*;
use super::diagnostics::{self, Diagnostic};
use super::lexer::{Lexer, Token};

pub type ParseError = Diagnostic;

/// Human-readable token name for error messages.
fn token_name(t: &Token) -> String {
    match t {
        Token::Match => "MATCH".into(),
        Token::Where => "WHERE".into(),
        Token::And => "AND".into(),
        Token::Or => "OR".into(),
        Token::Return => "RETURN".into(),
        Token::Similar => "SIMILAR".into(),
        Token::To => "TO".into(),
        Token::Score => "SCORE".into(),
        Token::Bm25 => "BM25".into(),
        Token::Using => "USING".into(),
        Token::Embedding => "EMBEDDING".into(),
        Token::Traverse => "TRAVERSE".into(),
        Token::Create => "CREATE".into(),
        Token::Update => "UPDATE".into(),
        Token::Delete => "DELETE".into(),
        Token::Ingest => "INGEST".into(),
        Token::Extract => "EXTRACT".into(),
        Token::Tables => "TABLES".into(),
        Token::Entities => "ENTITIES".into(),
        Token::Build => "BUILD".into(),
        Token::Relationships => "RELATIONSHIPS".into(),
        Token::Commit => "COMMIT".into(),
        Token::Explain => "EXPLAIN".into(),
        Token::AsOf => "AS_OF".into(),
        Token::Between => "BETWEEN".into(),
        Token::Historical => "HISTORICAL".into(),
        Token::Epistemic => "EPISTEMIC".into(),
        Token::Eq => "==".into(),
        Token::Neq => "!=".into(),
        Token::Lt => "<".into(),
        Token::Gt => ">".into(),
        Token::Lte => "<=".into(),
        Token::Gte => ">=".into(),
        Token::LParen => "(".into(),
        Token::RParen => ")".into(),
        Token::Comma => ",".into(),
        Token::Dot => ".".into(),
        Token::Star => "*".into(),
        Token::Ident(s) => format!("'{}'", s),
        Token::StringLit(s) => format!("\"{}\"", s),
        Token::Number(n) => n.to_string(),
        Token::Eof => "end of input".into(),
        Token::Error(e) => e.clone(),
    }
}

// Local helpers that wrap diagnostics constructors with token formatting.
fn unexpected(got: &Token, line: usize, col: usize) -> Diagnostic {
    diagnostics::unexpected_token(&token_name(got), line, col)
}
fn expected_err(desc: &str, got: &Token, line: usize, col: usize) -> Diagnostic {
    diagnostics::expected_token(desc, &token_name(got), line, col)
}
fn eof_err(line: usize, col: usize) -> Diagnostic {
    diagnostics::unexpected_eof(line, col)
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
        let line = lexer.last_span.line;
        let col = lexer.last_span.col;
        Parser {
            lexer,
            current,
            line,
            col,
        }
    }
    fn advance(&mut self) {
        self.current = self.lexer.next_token();
        self.line = self.lexer.last_span.line;
        self.col = self.lexer.last_span.col;
    }

    fn expect(&mut self, expected: Token) -> Result<(), ParseError> {
        if std::mem::discriminant(&self.current) == std::mem::discriminant(&expected) {
            self.advance();
            Ok(())
        } else {
            Err(expected_err(
                &format!("{:?}", expected),
                &self.current,
                self.line,
                self.col,
            ))
        }
    }

    fn expect_ident(&mut self, desc: &str) -> Result<String, ParseError> {
        match &self.current {
            Token::Ident(s) => {
                let v = s.clone();
                self.advance();
                Ok(v)
            }
            _ => Err(expected_err(desc, &self.current, self.line, self.col)),
        }
    }
    fn expect_string(&mut self) -> Result<String, ParseError> {
        match &self.current {
            Token::StringLit(s) => {
                let v = s.clone();
                self.advance();
                Ok(v)
            }
            _ => Err(expected_err(
                "string literal",
                &self.current,
                self.line,
                self.col,
            )),
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
        let mut preds = Vec::new();
        let mut sim = None;
        let mut trav = None;
        let mut temporal = None;
        let mut epistemic = None;
        loop {
            match &self.current {
                Token::Where | Token::And | Token::Or => {
                    self.advance();
                    preds.push(self.parse_predicate()?);
                }
                Token::Similar => {
                    self.advance();
                    self.expect(Token::To)?;
                    let query = self.expect_string()?;
                    let (score, using) = self.parse_similarity_options()?;
                    sim = Some(SimilarityClause {
                        query,
                        score,
                        using,
                    });
                }
                Token::Traverse => {
                    self.advance();
                    trav = Some(TraverseClause {
                        relation: self.expect_ident("relation name")?,
                    });
                }
                Token::AsOf | Token::Between | Token::Historical => {
                    if temporal.is_some() {
                        return Err(unexpected(&self.current, self.line, self.col)
                            .with_hint("duplicate temporal clause"));
                    }
                    temporal = Some(self.parse_temporal_clause()?);
                }
                Token::Epistemic => {
                    if epistemic.is_some() {
                        return Err(unexpected(&self.current, self.line, self.col)
                            .with_hint("duplicate epistemic clause"));
                    }
                    self.advance();
                    let mut allowed = vec![self.expect_ident("epistemic status")?];
                    while let Token::Comma = self.current {
                        self.advance();
                        allowed.push(self.expect_ident("epistemic status")?);
                    }
                    epistemic = Some(EpistemicClause { allowed });
                }
                Token::Return => {
                    self.advance();
                    return Ok(MatchStatement {
                        entity,
                        predicates: preds,
                        similarity: sim,
                        traverse: trav,
                        temporal,
                        epistemic,
                        projection: self.parse_projection()?,
                    });
                }
                Token::Eof => {
                    return Err(eof_err(self.line, self.col).with_hint("add RETURN clause"))
                }
                _ => {
                    return Err(unexpected(&self.current, self.line, self.col).with_hint(
                        "expected WHERE, SIMILAR, TRAVERSE, AS_OF, BETWEEN, HISTORICAL, EPISTEMIC, or RETURN",
                    ))
                }
            }
        }
    }

    /// v0.3 K2: `AS_OF <time>` | `BETWEEN <time> AND <time>` | `HISTORICAL`.
    fn parse_temporal_clause(&mut self) -> Result<TemporalClause, ParseError> {
        match self.current {
            Token::AsOf => {
                self.advance();
                Ok(TemporalClause::AsOf(self.parse_time_millis()?))
            }
            Token::Between => {
                self.advance();
                let from = self.parse_time_millis()?;
                self.expect(Token::And)?;
                let to = self.parse_time_millis()?;
                if from >= to {
                    return Err(diagnostics::invalid_temporal_range(self.line, self.col));
                }
                Ok(TemporalClause::Between { from, to })
            }
            Token::Historical => {
                self.advance();
                Ok(TemporalClause::Historical)
            }
            _ => unreachable!("parse_temporal_clause called on non-temporal token"),
        }
    }

    /// A time argument: epoch millis (`Number`, integer) or an ISO instant
    /// string `"YYYY-MM-DD"` / `"YYYY-MM-DDTHH:MM:SS"` (UTC).
    fn parse_time_millis(&mut self) -> Result<u64, ParseError> {
        match &self.current {
            Token::Number(n) => {
                let n = *n;
                self.advance();
                if n < 0.0 || n.fract() != 0.0 || n >= 9_007_199_254_740_992.0 {
                    return Err(expected_err(
                        "epoch millis (non-negative integer)",
                        &Token::Number(n),
                        self.line,
                        self.col,
                    ));
                }
                Ok(n as u64)
            }
            Token::StringLit(s) => {
                let s = s.clone();
                self.advance();
                iso_to_millis(&s).ok_or_else(|| {
                    expected_err(
                        "ISO instant \"YYYY-MM-DD\" or \"YYYY-MM-DDTHH:MM:SS\" (UTC)",
                        &Token::StringLit(s),
                        self.line,
                        self.col,
                    )
                })
            }
            _ => Err(expected_err(
                "time (epoch millis or ISO string)",
                &self.current,
                self.line,
                self.col,
            )),
        }
    }

    /// Parse optional `SCORE BM25` and/or `USING EMBEDDING` after `SIMILAR TO "..."`.
    fn parse_similarity_options(
        &mut self,
    ) -> Result<(Option<ScoringMethod>, Option<UsingMethod>), ParseError> {
        let mut score = None;
        let mut using = None;
        loop {
            match &self.current {
                Token::Score => {
                    self.advance();
                    match &self.current {
                        Token::Bm25 => {
                            score = Some(ScoringMethod::Bm25);
                            self.advance();
                        }
                        _ => return Err(expected_err("BM25", &self.current, self.line, self.col)),
                    }
                }
                Token::Using => {
                    self.advance();
                    match &self.current {
                        Token::Embedding => {
                            using = Some(UsingMethod::Embedding);
                            self.advance();
                        }
                        _ => {
                            return Err(expected_err(
                                "EMBEDDING",
                                &self.current,
                                self.line,
                                self.col,
                            ))
                        }
                    }
                }
                _ => break,
            }
        }
        Ok((score, using))
    }

    fn parse_predicate(&mut self) -> Result<Predicate, ParseError> {
        let prop = self.expect_ident("property name")?;
        let op = match &self.current {
            Token::Eq => "eq",
            Token::Neq => "neq",
            Token::Gt => "gt",
            Token::Lt => "lt",
            Token::Gte => "gte",
            Token::Lte => "lte",
            _ => {
                return Err(diagnostics::invalid_operator(
                    &token_name(&self.current),
                    self.line,
                    self.col,
                ))
            }
        };
        self.advance();
        let val = self.parse_expr()?;
        Ok(match op {
            "eq" => Predicate::Eq {
                property: prop,
                value: val,
            },
            "neq" => Predicate::Neq {
                property: prop,
                value: val,
            },
            "gt" => Predicate::Gt {
                property: prop,
                value: val,
            },
            "lt" => Predicate::Lt {
                property: prop,
                value: val,
            },
            "gte" => Predicate::Gte {
                property: prop,
                value: val,
            },
            "lte" => Predicate::Lte {
                property: prop,
                value: val,
            },
            _ => unreachable!(),
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        match &self.current {
            Token::StringLit(s) => {
                let v = s.clone();
                self.advance();
                Ok(Expr::String(v))
            }
            Token::Number(n) => {
                let v = *n;
                self.advance();
                Ok(Expr::Number(v))
            }
            Token::Ident(s) if s == "true" => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Token::Ident(s) if s == "false" => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            Token::Ident(s) if s == "null" => {
                self.advance();
                Ok(Expr::Null)
            }
            _ => Err(expected_err(
                "expression (string, number, bool, null)",
                &self.current,
                self.line,
                self.col,
            )),
        }
    }

    fn parse_projection(&mut self) -> Result<Projection, ParseError> {
        match &self.current {
            Token::Star => {
                self.advance();
                Ok(Projection::Star)
            }
            Token::Explain => {
                self.advance();
                Ok(Projection::Explain)
            }
            Token::Ident(_) => {
                let mut fields = vec![self.expect_ident("field name")?];
                while let Token::Comma = &self.current {
                    self.advance();
                    fields.push(self.expect_ident("field name")?);
                }
                Ok(Projection::Fields(fields))
            }
            _ => Err(expected_err(
                "*, explain, or field names",
                &self.current,
                self.line,
                self.col,
            )),
        }
    }

    fn parse_ingest(&mut self) -> Result<IngestStatement, ParseError> {
        self.expect(Token::Ingest)?;
        let source = self.expect_string()?;
        let (mut et, mut ee, mut br) = (false, false, false);
        loop {
            match &self.current {
                Token::Extract => {
                    self.advance();
                    match &self.current {
                        Token::Tables => {
                            et = true;
                            self.advance();
                        }
                        Token::Entities => {
                            ee = true;
                            self.advance();
                        }
                        _ => {
                            return Err(expected_err(
                                "TABLES or ENTITIES",
                                &self.current,
                                self.line,
                                self.col,
                            ))
                        }
                    }
                }
                Token::Build => {
                    self.advance();
                    self.expect(Token::Relationships)?;
                    br = true;
                }
                Token::Commit => {
                    self.advance();
                    return Ok(IngestStatement {
                        source,
                        extract_tables: et,
                        extract_entities: ee,
                        build_relationships: br,
                    });
                }
                _ => break,
            }
        }
        Ok(IngestStatement {
            source,
            extract_tables: et,
            extract_entities: ee,
            build_relationships: br,
        })
    }

    fn parse_create(&mut self) -> Result<CreateStatement, ParseError> {
        self.expect(Token::Create)?;
        let entity = self.expect_ident("entity name")?;
        let mut props = Vec::new();
        if let Token::Ident(_) = &self.current {
            loop {
                let k = self.expect_ident("property name")?;
                // If the next token is another ident or comma (not ==), the user
                // probably wrote "attr1, attr2" or "attributes name, ..." — missing values.
                if !matches!(&self.current, Token::Eq) {
                    return Err(
                        expected_err("==", &self.current, self.line, self.col).with_hint(format!(
                            "CREATE {} {} == <value>, ... — each property needs a value after ==",
                            entity, k
                        )),
                    );
                }
                self.expect(Token::Eq)?; // unreachable after the check above, kept for safety
                props.push((k, self.parse_expr()?));
                if !matches!(&self.current, Token::Comma) {
                    break;
                }
                self.advance();
            }
        }
        Ok(CreateStatement {
            entity,
            properties: props,
        })
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
                if !matches!(&self.current, Token::Comma) {
                    break;
                }
                self.advance();
            }
        }
        Ok(UpdateStatement {
            entity,
            koid,
            properties: props,
        })
    }

    fn parse_delete(&mut self) -> Result<DeleteStatement, ParseError> {
        self.expect(Token::Delete)?;
        Ok(DeleteStatement {
            entity: self.expect_ident("entity name")?,
            koid: self.expect_string()?,
        })
    }
}

// ---------------------------------------------------------------------------
// v0.3 K2: ISO instant parsing (UTC) — "YYYY-MM-DD" or "YYYY-MM-DDTHH:MM:SS"
// ---------------------------------------------------------------------------

/// Parse an ISO date/instant (UTC) to epoch millis. Strict: no fractional
/// seconds, no timezone suffixes; years 1970–9999.
fn iso_to_millis(s: &str) -> Option<u64> {
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some()
        || !(1970..=9999).contains(&y)
        || !(1..=12).contains(&m)
        || d == 0
        || d > days_in_month(y, m)
    {
        return None;
    }
    let days = days_from_civil(y, m, d);
    let (h, min, sec) = match time {
        None => (0u64, 0u64, 0u64),
        Some(t) => {
            let mut it = t.split(':');
            let h: u64 = it.next()?.parse().ok()?;
            let min: u64 = it.next()?.parse().ok()?;
            let sec: u64 = it.next()?.parse().ok()?;
            if it.next().is_some() || h > 23 || min > 59 || sec > 59 {
                return None;
            }
            (h, min, sec)
        }
    };
    Some((days as u64) * 86_400_000 + h * 3_600_000 + min * 60_000 + sec * 1_000)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // Mar = 0, ..., Feb = 11
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_match() {
        let mut p = Parser::new("MATCH Person RETURN *");
        match p.parse_statement().unwrap() {
            Statement::Match(m) => {
                assert_eq!(m.entity, "Person");
                assert_eq!(m.projection, Projection::Star);
            }
            _ => panic!(),
        }
    }
    #[test]
    fn parse_match_with_predicate() {
        match Parser::new("MATCH Person WHERE company == \"Visa\" RETURN *")
            .parse_statement()
            .unwrap()
        {
            Statement::Match(m) => assert_eq!(m.predicates.len(), 1),
            _ => panic!(),
        }
    }
    #[test]
    fn parse_match_similar_traverse() {
        match Parser::new("MATCH Person SIMILAR TO \"John\" TRAVERSE managed_by WHERE company == \"Visa\" RETURN explain").parse_statement().unwrap() {
            Statement::Match(m) => { assert!(m.similarity.is_some()); assert!(m.traverse.is_some()); }
            _ => panic!(),
        }
    }
    #[test]
    fn parse_update() {
        match Parser::new("UPDATE Person \"abc\" name == \"Bob\"")
            .parse_statement()
            .unwrap()
        {
            Statement::Update(u) => {
                assert_eq!(u.koid, "abc");
                assert_eq!(u.properties.len(), 1);
            }
            _ => panic!(),
        }
    }
    #[test]
    fn parse_delete() {
        match Parser::new("DELETE Person \"abc\"")
            .parse_statement()
            .unwrap()
        {
            Statement::Delete(d) => assert_eq!(d.koid, "abc"),
            _ => panic!(),
        }
    }
    #[test]
    fn parse_create() {
        match Parser::new("CREATE Person name == \"Alice\", age == 30")
            .parse_statement()
            .unwrap()
        {
            Statement::Create(c) => {
                assert_eq!(c.entity, "Person");
                assert_eq!(c.properties.len(), 2);
            }
            _ => panic!(),
        }
    }
    #[test]
    fn error_has_code_and_hint() {
        let e = Parser::new("BOGUS").parse_statement().unwrap_err();
        assert_eq!(e.code, diagnostics::Code::UnexpectedToken);
        assert!(e.format().contains("AIKOQL1010"));
        assert!(e.hint.is_some());
    }
    #[test]
    fn error_invalid_operator() {
        let e = Parser::new("MATCH Person WHERE name = \"x\" RETURN *")
            .parse_statement()
            .unwrap_err();
        assert_eq!(e.code, diagnostics::Code::InvalidOperator);
        assert!(e.hint.unwrap().contains("=="));
    }

    #[test]
    fn parse_similar_score_bm25() {
        match Parser::new("MATCH Person SIMILAR TO \"John\" SCORE BM25 RETURN *")
            .parse_statement()
            .unwrap()
        {
            Statement::Match(m) => {
                let sim = m.similarity.unwrap();
                assert_eq!(sim.query, "John");
                assert_eq!(sim.score, Some(ScoringMethod::Bm25));
                assert_eq!(sim.using, None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_similar_using_embedding() {
        match Parser::new("MATCH Doc SIMILAR TO \"concept\" USING EMBEDDING RETURN koid")
            .parse_statement()
            .unwrap()
        {
            Statement::Match(m) => {
                let sim = m.similarity.unwrap();
                assert_eq!(sim.query, "concept");
                assert_eq!(sim.score, None);
                assert_eq!(sim.using, Some(UsingMethod::Embedding));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_similar_both_bm25_and_embedding() {
        match Parser::new("MATCH X SIMILAR TO \"q\" SCORE BM25 USING EMBEDDING RETURN *")
            .parse_statement()
            .unwrap()
        {
            Statement::Match(m) => {
                let sim = m.similarity.unwrap();
                assert_eq!(sim.score, Some(ScoringMethod::Bm25));
                assert_eq!(sim.using, Some(UsingMethod::Embedding));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_similar_reordered_using_then_score() {
        // USING EMBEDDING before SCORE BM25 should also work.
        match Parser::new("MATCH X SIMILAR TO \"q\" USING EMBEDDING SCORE BM25 RETURN *")
            .parse_statement()
            .unwrap()
        {
            Statement::Match(m) => {
                let sim = m.similarity.unwrap();
                assert_eq!(sim.score, Some(ScoringMethod::Bm25));
                assert_eq!(sim.using, Some(UsingMethod::Embedding));
            }
            _ => panic!(),
        }
    }

    // ---- v0.3 K2: temporal + epistemic clauses ----

    fn parse_match(source: &str) -> MatchStatement {
        match Parser::new(source).parse_statement().unwrap() {
            Statement::Match(m) => m,
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn parse_match_as_of_epoch_millis() {
        let m = parse_match("MATCH Person AS_OF 1735689600000 RETURN *");
        assert_eq!(m.temporal, Some(TemporalClause::AsOf(1_735_689_600_000)));
        assert_eq!(m.epistemic, None);
    }

    #[test]
    fn parse_match_as_of_iso_date() {
        let m = parse_match("MATCH Person AS_OF \"2026-01-01\" RETURN *");
        assert_eq!(m.temporal, Some(TemporalClause::AsOf(1_767_225_600_000)));
    }

    #[test]
    fn parse_match_as_of_iso_instant() {
        let m = parse_match("MATCH Person AS_OF \"2026-08-19T12:30:45\" RETURN *");
        assert_eq!(m.temporal, Some(TemporalClause::AsOf(1_787_142_645_000)));
    }

    #[test]
    fn parse_match_between() {
        let m = parse_match("MATCH Person BETWEEN 1000 AND 2000 RETURN *");
        assert_eq!(
            m.temporal,
            Some(TemporalClause::Between {
                from: 1_000,
                to: 2_000,
            })
        );
    }

    #[test]
    fn parse_match_between_rejects_non_increasing_range() {
        let e = Parser::new("MATCH Person BETWEEN 2000 AND 1000 RETURN *")
            .parse_statement()
            .unwrap_err();
        assert_eq!(e.code, diagnostics::Code::InvalidTemporalRange);
    }

    #[test]
    fn parse_match_historical() {
        let m = parse_match("MATCH Person HISTORICAL RETURN *");
        assert_eq!(m.temporal, Some(TemporalClause::Historical));
    }

    #[test]
    fn parse_match_epistemic_filter() {
        let m = parse_match("MATCH Fact EPISTEMIC verified, asserted RETURN *");
        assert_eq!(
            m.epistemic,
            Some(EpistemicClause {
                allowed: vec!["verified".into(), "asserted".into()],
            })
        );
    }

    #[test]
    fn parse_match_duplicate_temporal_is_rejected() {
        assert!(Parser::new("MATCH Person AS_OF 1000 AS_OF 2000 RETURN *")
            .parse_statement()
            .is_err());
    }

    #[test]
    fn parse_match_temporal_before_where() {
        // Clause order is free — temporal before WHERE parses fine.
        let m = parse_match("MATCH Person AS_OF 1000 WHERE company == \"Visa\" RETURN *");
        assert_eq!(m.temporal, Some(TemporalClause::AsOf(1_000)));
        assert_eq!(m.predicates.len(), 1);
    }

    #[test]
    fn parse_time_millis_rejects_bad_iso() {
        for bad in [
            "MATCH X AS_OF \"2026-13-01\" RETURN *",          // month 13
            "MATCH X AS_OF \"2026-02-30\" RETURN *",          // Feb 30
            "MATCH X AS_OF \"1969-01-01\" RETURN *",          // pre-epoch
            "MATCH X AS_OF \"2026-01-01T25:00:00\" RETURN *", // hour 25
            "MATCH X AS_OF \"2026-01-01T12:00\" RETURN *",    // missing seconds
            "MATCH X AS_OF \"2026-01-01T12:00:00.5\" RETURN *", // fractional seconds
        ] {
            assert!(
                Parser::new(bad).parse_statement().is_err(),
                "expected rejection: {}",
                bad
            );
        }
    }

    #[test]
    fn parse_time_millis_rejects_fractional_epoch() {
        assert!(Parser::new("MATCH X AS_OF 12.5 RETURN *")
            .parse_statement()
            .is_err());
    }

    #[test]
    fn iso_to_millis_known_dates() {
        assert_eq!(iso_to_millis("1970-01-01"), Some(0));
        assert_eq!(iso_to_millis("1970-01-02"), Some(86_400_000));
        assert_eq!(iso_to_millis("2000-01-01"), Some(946_684_800_000));
        // 2000 is a leap year — Feb 29 exists.
        assert_eq!(iso_to_millis("2000-03-01"), Some(951_868_800_000));
        // 2100 is NOT a leap year (century rule).
        assert_eq!(iso_to_millis("2100-03-01"), Some(4_107_542_400_000));
        assert_eq!(iso_to_millis("2000-02-30"), None);
        assert_eq!(iso_to_millis("not-a-date"), None);
    }
}
