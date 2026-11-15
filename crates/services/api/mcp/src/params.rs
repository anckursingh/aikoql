//! Parameters in program text: `{{name}}` placeholders in a program body are
//! filled from a parameter map, by `execute_program` (the caller's `params`)
//! and `execute_agent` (the agent's `prompt`).
//!
//! The values are data. They are pasted into text that the AikoQL lexer then
//! reads, and the lexer has no escape syntax (a string literal ends at the
//! first `"`), so a value cannot be quoted: it has to be refused when it
//! would not stay a single value where it lands. The rule, decided with the
//! real lexer on the text built so far:
//!
//! - **inside a string literal** the value may be any text without `"`;
//! - **anywhere else** (a bare operand, a property name, a comment) the value
//!   must lex as exactly one number, or as an identifier path (`a`, `a.b`).
//!   Free text, keywords, operators, quotes, spaces and newlines are refused.
//!
//! One pass over the template: a value is never searched for placeholders, so
//! a value that contains `{{other}}` is not expanded again.

use crate::http::value_to_string;
use crate::Value;
use aikoql_compiler::parser::lexer::{Lexer, Token};
use std::collections::BTreeMap;

/// The tokens of `src`, up to and including the first `Eof` or `Error`.
fn tokens(src: &str) -> Vec<Token> {
    let mut lexer = Lexer::new(src);
    let mut out = Vec::new();
    loop {
        let t = lexer.next_token();
        let end = matches!(t, Token::Eof | Token::Error(_));
        out.push(t);
        if end {
            return out;
        }
    }
}

/// Does `prefix` end inside an open string literal (as the lexer reads it)?
fn inside_literal(prefix: &str) -> bool {
    matches!(
        tokens(prefix).last(),
        Some(Token::Error(m)) if m == "unterminated string"
    )
}

/// One number, or an identifier path (`a`, `a.b`): a value that stays one
/// operand wherever it is pasted.
fn plain_operand(text: &str) -> bool {
    // Only operand characters: the lexer would read `0//x` as the number 0
    // followed by a comment that swallows the rest of the line, so one token
    // is not enough — every character must belong to the operand.
    if text.is_empty()
        || !text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return false;
    }
    let t = tokens(text);
    match t.as_slice() {
        [Token::Number(_), Token::Eof] => true,
        [Token::Ident(_), rest @ ..] => {
            let mut rest = rest;
            while let [Token::Dot, Token::Ident(_), tail @ ..] = rest {
                rest = tail;
            }
            matches!(rest, [Token::Eof])
        }
        _ => false,
    }
}

fn value_text(name: &str, value: &Value, in_literal: bool) -> Result<String, String> {
    let text = value_to_string(value);
    if in_literal {
        if text.contains('"') {
            return Err(format!(
                "parameter '{name}' contains a double quote, which an AikoQL string literal \
                 cannot hold (it would end the literal)"
            ));
        }
    } else if !plain_operand(&text) {
        return Err(format!(
            "parameter '{name}' is not a plain number or identifier, so it cannot be used \
             outside a string literal"
        ));
    }
    Ok(text)
}

/// Fill the `{{name}}` placeholders of `body` from `params`. A placeholder
/// with no parameter is left as written. An error names the first parameter
/// that cannot be used where it lands; nothing is substituted then.
pub(crate) fn substitute_params(
    body: &str,
    params: &BTreeMap<String, Value>,
) -> Result<String, String> {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let value = after
            .find("}}")
            .and_then(|end| params.get(&after[..end]).map(|v| (&after[..end], end, v)));
        match value {
            Some((name, end, v)) => {
                out.push_str(&value_text(name, v, inside_literal(&out))?);
                rest = &after[end + 2..];
            }
            None => {
                // not a known placeholder: keep one brace and look again, so
                // `{{{who}}}` still finds `{{who}}` inside it
                out.push('{');
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod fuzz_support {
    //! A seeded generator for the fuzz tests (no external randomness: every
    //! failure replays from its seed).

    use crate::Value;

    pub(crate) struct Rng(pub u64);

    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            // splitmix64
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        pub(crate) fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Pieces that have broken text-built queries before: quotes, escapes,
    /// comment starters, braces (including placeholder look-alikes), keywords,
    /// operators, whitespace, look-alike quotes, NUL, multi-byte text.
    pub(crate) const NASTY: &[&str] = &[
        "\"",
        "\\",
        "'",
        "`",
        ";",
        "//",
        "--",
        "/*",
        "*/",
        "{{",
        "}}",
        "{{who}}",
        "{{n}}",
        "{{field}}",
        "}}{{",
        "\n",
        "\r\n",
        "\t",
        " ",
        "==",
        "!=",
        "<=",
        "(",
        ")",
        ",",
        ".",
        "*",
        "OR",
        "or",
        "AND",
        "RETURN",
        "DELETE",
        "MATCH",
        "WHERE",
        "UPDATE",
        "name",
        "a.b",
        "\u{201c}",
        "\u{201d}",
        "\u{ff02}",
        "\u{0}",
        "é",
        "日本",
        "0",
        "12.5",
        "-1",
        "1e9",
    ];

    pub(crate) fn text(rng: &mut Rng) -> String {
        let pieces = rng.below(9);
        let mut s = String::new();
        for _ in 0..pieces {
            match rng.below(10) {
                0..=5 => s.push_str(NASTY[rng.below(NASTY.len())]),
                6..=7 => {
                    for _ in 0..1 + rng.below(6) {
                        s.push((b'a' + rng.below(26) as u8) as char);
                    }
                }
                8 => s.push(char::from_u32(0x20 + rng.below(0x5f) as u32).unwrap_or('?')),
                _ => s.push(char::from_u32(rng.next() as u32 % 0x11_0000).unwrap_or('\u{fffd}')),
            }
        }
        s
    }

    pub(crate) fn value(rng: &mut Rng) -> Value {
        match rng.below(12) {
            0..=6 => Value::Text(text(rng)),
            7 => Value::Int(rng.next() as i64 % 1_000_000 - 500_000),
            8 => Value::Float((rng.next() % 10_000) as f64 / 7.0),
            9 => Value::Bool(rng.below(2) == 0),
            10 => Value::Null,
            _ => Value::Bytes(vec![rng.next() as u8; rng.below(4)]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fuzz_support::{text, value, Rng};
    use super::*;

    /// (template, the placeholder names in it). A `"..."` around a placeholder
    /// makes it a literal position; no quotes makes it bare; a comment line
    /// makes it neither (a newline would end the comment).
    const TEMPLATES: &[(&str, &[&str])] = &[
        (r#"MATCH Doc WHERE name == "{{who}}" RETURN *"#, &["who"]),
        ("MATCH Doc WHERE age > {{n}} RETURN *", &["n"]),
        (
            r#"MATCH Doc WHERE name == "{{who}}" AND city == "{{city}}" RETURN *"#,
            &["who", "city"],
        ),
        (
            r#"MATCH Doc WHERE {{field}} == "{{who}}" RETURN *"#,
            &["field", "who"],
        ),
        (
            r#"MATCH Doc WHERE name == "{{who}}" OR alias == "{{who}}" RETURN *"#,
            &["who"],
        ),
        (
            "// said \"hi\n MATCH Doc WHERE name == \"{{who}}\" RETURN *",
            &["who"],
        ),
        ("// {{who}}\nMATCH Doc RETURN *", &["who"]),
        (r#"MATCH Doc WHERE name == "x{{who}}y" RETURN *"#, &["who"]),
    ];

    fn params_of(names: &[&str], rng: &mut Rng) -> BTreeMap<String, Value> {
        names.iter().map(|n| (n.to_string(), value(rng))).collect()
    }

    /// The independent oracle: lex the template with each placeholder replaced
    /// by a marker, then put each value in as the ONE thing it may become —
    /// a string literal holding its text, or the value's own operand tokens.
    /// The substituted text must lex to exactly that stream. This does not
    /// look at `substitute_params`'s rules, only at the lexer.
    fn structure_preserved(
        template: &str,
        names: &[&str],
        params: &BTreeMap<String, Value>,
        out: &str,
    ) -> bool {
        let mut marked = template.to_string();
        for n in names {
            marked = marked.replace(&format!("{{{{{n}}}}}"), &format!("ZQ{n}ZQ"));
        }
        let mut expected = Vec::new();
        for t in tokens(&marked) {
            match &t {
                Token::StringLit(s) | Token::Ident(s) if s.contains("ZQ") => {
                    // a literal or bare operand carrying markers: rebuild it
                    // from the values it stands for
                    let mut rebuilt = s.clone();
                    for n in names {
                        let v = value_to_string(&params[*n]);
                        rebuilt = rebuilt.replace(&format!("ZQ{n}ZQ"), &v);
                    }
                    match &t {
                        Token::StringLit(_) => expected.push(Token::StringLit(rebuilt)),
                        _ => {
                            let mut vt = tokens(&rebuilt);
                            vt.pop(); // the Eof
                            expected.extend(vt);
                        }
                    }
                }
                _ => expected.push(t),
            }
        }
        tokens(out) == expected
    }

    #[test]
    fn a_plain_value_is_pasted_where_it_belongs() {
        let mut p = BTreeMap::new();
        p.insert("who".to_string(), Value::Text("Alice".into()));
        p.insert("n".to_string(), Value::Int(42));
        p.insert("field".to_string(), Value::Text("profile.age".into()));
        assert_eq!(
            substitute_params(r#"MATCH Doc WHERE name == "{{who}}" RETURN *"#, &p).unwrap(),
            r#"MATCH Doc WHERE name == "Alice" RETURN *"#
        );
        assert_eq!(
            substitute_params("MATCH Doc WHERE age > {{n}} RETURN *", &p).unwrap(),
            "MATCH Doc WHERE age > 42 RETURN *"
        );
        assert_eq!(
            substitute_params("MATCH Doc WHERE {{field}} == 1 RETURN *", &p).unwrap(),
            "MATCH Doc WHERE profile.age == 1 RETURN *"
        );
        // text with spaces, apostrophes and punctuation is fine inside a literal
        p.insert(
            "who".to_string(),
            Value::Text("O'Brien, Jr. (the 2nd)".into()),
        );
        assert!(substitute_params(r#"name == "{{who}}""#, &p).is_ok());
    }

    #[test]
    fn a_value_that_would_change_the_query_is_refused() {
        let mut p = BTreeMap::new();
        // closes the literal and adds a clause
        p.insert(
            "who".to_string(),
            Value::Text("Alice\" OR name == \"Bob".into()),
        );
        assert!(substitute_params(r#"name == "{{who}}""#, &p).is_err());
        // free text in a bare position
        p.insert("n".to_string(), Value::Text("1 OR 1 == 1".into()));
        assert!(substitute_params("age > {{n}}", &p).is_err());
        // a keyword is one token, but not an operand
        p.insert("n".to_string(), Value::Text("OR".into()));
        assert!(substitute_params("age > {{n}}", &p).is_err());
        // a newline would end a comment
        p.insert("who".to_string(), Value::Text("x\nDELETE Doc".into()));
        assert!(substitute_params("// {{who}}\nMATCH Doc RETURN *", &p).is_err());
        // a placeholder-shaped value is data, not expanded again
        p.insert("a".to_string(), Value::Text("{{b}}".into()));
        p.insert("b".to_string(), Value::Text("SECOND".into()));
        assert_eq!(
            substitute_params(r#"x == "{{a}}""#, &p).unwrap(),
            r#"x == "{{b}}""#
        );
    }

    #[test]
    fn unknown_and_malformed_placeholders_are_left_alone() {
        let p = BTreeMap::new();
        for s in ["{{nope}}", "{{", "}}", "{{who", "a{{b}}c", "{{{x}}}", "{}"] {
            assert_eq!(substitute_params(s, &p).unwrap(), s);
        }
        let mut p = BTreeMap::new();
        p.insert("who".to_string(), Value::Text("A".into()));
        assert_eq!(
            substitute_params(r#""{{{who}}}""#, &p).unwrap(),
            r#""{A}""#,
            "triple braces still find the placeholder, as a plain replace did"
        );
    }

    /// The fuzz: for every template, random values (nasty text, numbers,
    /// bools, null, bytes), many seeds. If `substitute_params` accepts, the
    /// result must lex to the template's stream with each value as ONE thing;
    /// and a literal position must accept every value that has no `"`.
    #[test]
    fn fuzz_a_value_never_changes_the_shape_of_the_query() {
        let mut checked = 0usize;
        let mut accepted = 0usize;
        for seed in [1u64, 2, 3, 0xA11CE, 0xB0B, 0xDEAD_BEEF] {
            let mut rng = Rng(seed);
            for _ in 0..4_000 {
                for (template, names) in TEMPLATES {
                    let params = params_of(names, &mut rng);
                    checked += 1;
                    match substitute_params(template, &params) {
                        Ok(out) => {
                            accepted += 1;
                            assert!(
                                structure_preserved(template, names, &params, &out),
                                "seed {seed}: a value changed the query\n  template: {template:?}\n  \
                                 params: {params:?}\n  output: {out:?}"
                            );
                        }
                        Err(_) => {
                            // a refusal must have a reason an outsider can
                            // see: some value carries a quote or is not one
                            // plain operand (`value_text` decides which)
                            assert!(
                                names.iter().any(|n| {
                                    let t = value_to_string(&params[*n]);
                                    t.contains('"') || !plain_operand(&t)
                                }),
                                "seed {seed}: refused with nothing wrong: {template:?} {params:?}"
                            );
                        }
                    }
                }
            }
        }
        assert!(
            checked > 100_000 && accepted > 10_000,
            "{checked} {accepted}"
        );
    }

    /// Text without a quote is always welcome in a string literal.
    #[test]
    fn fuzz_quote_free_text_is_always_accepted_in_a_literal() {
        let mut rng = Rng(77);
        for _ in 0..40_000 {
            let t = text(&mut rng).replace('"', "");
            let mut p = BTreeMap::new();
            p.insert("who".to_string(), Value::Text(t.clone()));
            let out = substitute_params(r#"MATCH Doc WHERE name == "{{who}}" RETURN *"#, &p)
                .unwrap_or_else(|e| panic!("refused {t:?}: {e}"));
            assert!(structure_preserved(
                r#"MATCH Doc WHERE name == "{{who}}" RETURN *"#,
                &["who"],
                &p,
                &out
            ));
        }
    }

    /// The old behaviour, kept only here: plain text replacement.
    fn naive_substitute(body: &str, params: &BTreeMap<String, Value>) -> String {
        let mut q = body.to_string();
        for (k, v) in params {
            q = q.replace(&format!("{{{{{k}}}}}"), &value_to_string(v));
        }
        q
    }

    /// The suite is not vacuous: the same oracle flags the old behaviour on a
    /// small adversarial corpus, and `substitute_params` passes the same corpus.
    #[test]
    fn the_oracle_catches_the_old_plain_replace() {
        let corpus = [
            "Alice\" OR name == \"Bob",
            "x\" OR \"1\" == \"1",
            "\" RETURN * // ",
            "a\" AND name != \"b",
            "{{who}}\" OR name == \"z",
        ];
        let template = r#"MATCH Doc WHERE name == "{{who}}" RETURN *"#;
        let (mut naive_caught, mut ours_caught) = (0, 0);
        for payload in corpus {
            let mut p = BTreeMap::new();
            p.insert("who".to_string(), Value::Text(payload.into()));
            if !structure_preserved(template, &["who"], &p, &naive_substitute(template, &p)) {
                naive_caught += 1;
            }
            if let Ok(out) = substitute_params(template, &p) {
                if !structure_preserved(template, &["who"], &p, &out) {
                    ours_caught += 1;
                }
            }
        }
        assert_eq!(
            naive_caught,
            corpus.len(),
            "the oracle must flag every payload"
        );
        assert_eq!(ours_caught, 0, "substitute_params must never produce one");
    }
}
