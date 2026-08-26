//! MRFC-0070 Phase A2: Code-to-Knowledge Compiler — Python/TypeScript/Java.
//!
//! Parses source files via per-language tree-sitter grammars and produces the
//! same typed KnowledgeIr contract as the Rust compiler (code.rs):
//! classes/functions/interfaces → entities, imports → DEPENDS_ON,
//! extends/implements → IMPLEMENTS, test markers → TESTED_BY,
//! docstrings/JSDoc/Javadoc → Claim facts.
//!
//! Parse failure = empty IR + stderr warning, never an error (same rule as
//! the syn path). Deterministic: same input → same IR.
//!
//! ponytail: no recursion into function bodies (same as the Rust compiler's
//! item-level walk) — imports or nested defs inside a function body produce
//! nothing. Add a body walk if a corpus needs it.

use crate::ir::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};
use aikoql_kernel::knowledge::kom;
use tree_sitter::{Node, Parser};

// ---------------------------------------------------------------------------
// Public API (mirrors code.rs: file + source string entry points)
// ---------------------------------------------------------------------------

pub fn compile_python_file(path: &str) -> Result<KnowledgeIr, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read '{}': {}", path, e))?;
    Ok(compile_python_source(&src, Some(path)))
}

pub fn compile_python_source(src: &str, file_hint: Option<&str>) -> KnowledgeIr {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    compile_with_grammar(
        src,
        file_hint,
        "python-code-parser",
        "tree-sitter-python",
        &lang,
        walk_python,
    )
}

pub fn compile_ts_file(path: &str) -> Result<KnowledgeIr, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read '{}': {}", path, e))?;
    let lower = path.to_lowercase();
    Ok(compile_ts_source_with_grammar(
        &src,
        Some(path),
        lower.ends_with(".tsx") || lower.ends_with(".jsx"),
    ))
}

pub fn compile_ts_source(src: &str, file_hint: Option<&str>) -> KnowledgeIr {
    compile_ts_source_with_grammar(src, file_hint, false)
}

fn compile_ts_source_with_grammar(src: &str, file_hint: Option<&str>, tsx: bool) -> KnowledgeIr {
    let lang: tree_sitter::Language = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    };
    compile_with_grammar(
        src,
        file_hint,
        "typescript-code-parser",
        "tree-sitter-typescript",
        &lang,
        walk_ts,
    )
}

pub fn compile_java_file(path: &str) -> Result<KnowledgeIr, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read '{}': {}", path, e))?;
    Ok(compile_java_source(&src, Some(path)))
}

pub fn compile_java_source(src: &str, file_hint: Option<&str>) -> KnowledgeIr {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    compile_with_grammar(
        src,
        file_hint,
        "java-code-parser",
        "tree-sitter-java",
        &lang,
        walk_java,
    )
}

// ---------------------------------------------------------------------------
// Shared harness + helpers
// ---------------------------------------------------------------------------

/// Parse with a grammar and walk the tree. Parse/grammar failure → empty IR
/// with a stderr warning (never an error — same contract as the syn path).
fn compile_with_grammar<F>(
    src: &str,
    file_hint: Option<&str>,
    extractor: &str,
    model: &str,
    lang: &tree_sitter::Language,
    walk: F,
) -> KnowledgeIr
where
    F: FnOnce(Node, &str, &mut KnowledgeIr, &str, &str),
{
    let document_id = file_hint.map(|s| s.to_string());
    let mut ir = KnowledgeIr {
        document_id: document_id.clone(),
        extractor: extractor.to_string(),
        ..Default::default()
    };

    let mut parser = Parser::new();
    if let Err(e) = parser.set_language(lang) {
        eprintln!("{}: set_language failed (ABI mismatch): {}", extractor, e);
        return ir;
    }
    match parser.parse(src, None) {
        Some(tree) => {
            walk(tree.root_node(), src, &mut ir, extractor, model);
            // Provenance pass: candidate builders can't see the file path, so
            // fill evidence.document_id here (same as code.rs).
            if let Some(doc) = &document_id {
                for e in &mut ir.entities {
                    e.evidence.document_id = Some(doc.clone());
                }
                for r in &mut ir.relations {
                    r.evidence.document_id = Some(doc.clone());
                }
            }
        }
        None => eprintln!("{}: parse failed", extractor),
    }
    ir
}

/// Named children of a node (tree-sitter 0.25 removed Node::children — the
/// TreeCursor is the iteration API).
fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let n = cursor.node();
            if n.is_named() {
                out.push(n);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    out
}

fn text_of(node: Node, src: &str) -> String {
    node.utf8_text(src.as_bytes()).unwrap_or("").to_string()
}

fn ts_evidence(extractor: &str, model: &str) -> Evidence {
    Evidence {
        document_id: None,
        page: None,
        source: None,
        extractor: extractor.to_string(),
        model: Some(model.to_string()),
        confidence: 0.85,
    }
}

fn push_entity(
    ir: &mut KnowledgeIr,
    name: &str,
    type_hint: &str,
    node: Node,
    docs: &[String],
    src: &str,
    extractor: &str,
    model: &str,
) {
    // Doc lines when present, else the node's first source line — a doc-less
    // entity still gets one lexical mention (the code.rs fn_mentions rule).
    let mentions = if docs.is_empty() {
        text_of(node, src)
            .lines()
            .take(1)
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    } else {
        docs.to_vec()
    };
    ir.entities.push(EntityCandidate {
        name: name.to_string(),
        type_hint: Some(type_hint.to_string()),
        mentions,
        confidence: 0.85,
        evidence: ts_evidence(extractor, model),
    });
    for d in docs {
        ir.facts.push(FactCandidate {
            snippet: None,
            statement: d.clone(),
            entities: vec![name.to_string()],
            confidence: 0.75,
            evidence: ts_evidence(extractor, model),
        });
    }
}

fn push_depends_on(
    ir: &mut KnowledgeIr,
    subject: &str,
    object: &str,
    extractor: &str,
    model: &str,
) {
    ir.relations.push(RelationCandidate {
        subject: subject.to_string(),
        predicate: kom::DEPENDS_ON.to_string(),
        object: object.to_string(),
        confidence: 0.85,
        evidence: ts_evidence(extractor, model),
    });
}

fn push_implements(
    ir: &mut KnowledgeIr,
    subject: &str,
    object: &str,
    extractor: &str,
    model: &str,
) {
    ir.relations.push(RelationCandidate {
        subject: subject.to_string(),
        predicate: kom::IMPLEMENTS.to_string(),
        object: object.to_string(),
        confidence: 0.8,
        evidence: ts_evidence(extractor, model),
    });
}

fn push_tested_by(ir: &mut KnowledgeIr, subject: &str, object: &str, extractor: &str, model: &str) {
    ir.relations.push(RelationCandidate {
        subject: subject.to_string(),
        predicate: kom::TESTED_BY.to_string(),
        object: object.to_string(),
        confidence: 0.8,
        evidence: ts_evidence(extractor, model),
    });
}

/// Collect type name texts at depth ≤ `depth` (bounds the generics case:
/// `extends Base<T>` yields Base, not T). The TS grammar labels a plain
/// extends target `identifier` and an implements target `type_identifier`;
/// Java uses `type_identifier` for both.
fn collect_type_ids(node: Node, src: &str, out: &mut Vec<String>, depth: u8) {
    if depth == 0 {
        return;
    }
    for c in named_children(node) {
        if c.kind() == "type_identifier" || c.kind() == "identifier" {
            out.push(text_of(c, src));
        } else {
            collect_type_ids(c, src, out, depth - 1);
        }
    }
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

fn walk_python(root: Node, src: &str, ir: &mut KnowledgeIr, extractor: &str, model: &str) {
    let children = named_children(root);
    // Module docstring: first statement string literal at module level
    // (mirrors the Rust crate-doc arm in code.rs).
    if let Some(first) = children.first() {
        if first.kind() == "expression_statement" {
            for c in named_children(*first) {
                if c.kind() == "string" {
                    for line in strip_py_string(&text_of(c, src)) {
                        ir.facts.push(FactCandidate {
                            snippet: None,
                            statement: line,
                            entities: vec!["module".into()],
                            confidence: 0.8,
                            evidence: ts_evidence(extractor, model),
                        });
                    }
                }
            }
        }
    }
    for node in children {
        walk_python_node(node, src, ir, extractor, model, None);
    }
}

fn walk_python_node(
    node: Node,
    src: &str,
    ir: &mut KnowledgeIr,
    extractor: &str,
    model: &str,
    parent: Option<&str>,
) {
    // decorated_definition wraps the real def — resolve before matching.
    let target = match node.kind() {
        "decorated_definition" => node.child_by_field_name("definition").unwrap_or(node),
        _ => node,
    };
    match target.kind() {
        "class_definition" | "function_definition" => {
            let name = target
                .child_by_field_name("name")
                .map(|n| text_of(n, src))
                .unwrap_or_default();
            if name.is_empty() {
                return;
            }
            let full = match parent {
                Some(p) => format!("{}::{}", p, name),
                None => name.clone(),
            };
            let docs = python_docstring(target, src);
            let is_test = target.kind() == "function_definition" && name.starts_with("test_");
            let hint = match (target.kind(), parent) {
                ("class_definition", _) => "Class",
                ("function_definition", Some(_)) => "Method",
                ("function_definition", None) if is_test => "Test",
                ("function_definition", None) => "Function",
                _ => unreachable!(),
            };
            push_entity(ir, &full, hint, target, &docs, src, extractor, model);
            if is_test {
                push_tested_by(ir, &full, parent.unwrap_or("module"), extractor, model);
            }
            // Recurse class bodies (methods, nested classes). Function bodies
            // are not walked (ponytail note at module top).
            if target.kind() == "class_definition" {
                if let Some(body) = target.child_by_field_name("body") {
                    for b in named_children(body) {
                        walk_python_node(b, src, ir, extractor, model, Some(&full));
                    }
                }
            }
        }
        "import_statement" => {
            let subject = parent.unwrap_or("module");
            for c in named_children(target) {
                match c.kind() {
                    "dotted_name" => {
                        push_depends_on(ir, subject, &text_of(c, src), extractor, model);
                    }
                    "aliased_import" => {
                        let obj = c
                            .child_by_field_name("name")
                            .or_else(|| c.child_by_field_name("alias"))
                            .unwrap_or(c);
                        push_depends_on(ir, subject, &text_of(obj, src), extractor, model);
                    }
                    _ => {}
                }
            }
        }
        "import_from_statement" => {
            if let Some(m) = target.child_by_field_name("module_name") {
                let obj = text_of(m, src);
                // Relative imports (`from . import x`) name only dots — skip.
                if !obj.is_empty() && !obj.chars().all(|c| c == '.') {
                    push_depends_on(ir, parent.unwrap_or("module"), &obj, extractor, model);
                }
            }
        }
        _ => {}
    }
}

/// First statement of a body block, when it is a bare string literal.
fn python_docstring(node: Node, src: &str) -> Vec<String> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    for first in named_children(body).into_iter().take(1) {
        if first.kind() != "expression_statement" {
            break;
        }
        for c in named_children(first) {
            if c.kind() == "string" {
                return strip_py_string(&text_of(c, src));
            }
        }
    }
    Vec::new()
}

/// Strip string-literal delimiters (incl. r/f/u prefixes and triple quotes).
fn strip_py_string(raw: &str) -> Vec<String> {
    let start = raw.find(['"', '\'']).unwrap_or(0);
    let s = &raw[start..];
    let body = if (s.starts_with("\"\"\"") && s.len() >= 6 && s.ends_with("\"\"\""))
        || (s.starts_with("'''") && s.len() >= 6 && s.ends_with("'''"))
    {
        &s[3..s.len() - 3]
    } else if s.len() >= 2 {
        let b = s.as_bytes();
        let (first, last) = (b[0], b[b.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            &s[1..s.len() - 1]
        } else {
            s
        }
    } else {
        s
    };
    body.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

// ---------------------------------------------------------------------------
// TypeScript / JavaScript
// ---------------------------------------------------------------------------

fn walk_ts(root: Node, src: &str, ir: &mut KnowledgeIr, extractor: &str, model: &str) {
    for node in named_children(root) {
        walk_ts_node(node, src, ir, extractor, model, None);
    }
}

fn walk_ts_node(
    node: Node,
    src: &str,
    ir: &mut KnowledgeIr,
    extractor: &str,
    model: &str,
    parent: Option<&str>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text_of(n, src))
                .unwrap_or_default();
            if name.is_empty() {
                return;
            }
            let full = match parent {
                Some(p) => format!("{}::{}", p, name),
                None => name.clone(),
            };
            let hint = if node.kind() == "class_declaration" {
                "Class"
            } else {
                "Interface"
            };
            let docs = ts_jsdoc(node, src);
            push_entity(ir, &full, hint, node, &docs, src, extractor, model);
            // extends/implements → IMPLEMENTS (heritage clause carries both).
            for c in named_children(node) {
                if c.kind() == "class_heritage" || c.kind() == "extends_type_clause" {
                    let mut targets = Vec::new();
                    collect_type_ids(c, src, &mut targets, 2);
                    for t in targets {
                        push_implements(ir, &full, &t, extractor, model);
                    }
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                for b in named_children(body) {
                    walk_ts_node(b, src, ir, extractor, model, Some(&full));
                }
            }
        }
        "function_declaration" | "method_definition" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text_of(n, src))
                .unwrap_or_default();
            if name.is_empty() {
                return;
            }
            let full = match parent {
                Some(p) => format!("{}::{}", p, name),
                None => name.clone(),
            };
            let hint = if parent.is_some() {
                "Method"
            } else {
                "Function"
            };
            let docs = ts_jsdoc(node, src);
            push_entity(ir, &full, hint, node, &docs, src, extractor, model);
        }
        "import_statement" => {
            if let Some(s) = node.child_by_field_name("source") {
                let obj = strip_ts_string(&text_of(s, src));
                if !obj.is_empty() {
                    push_depends_on(ir, parent.unwrap_or("module"), &obj, extractor, model);
                }
            }
        }
        "call_expression" => {
            let Some(f) = node.child_by_field_name("function") else {
                return;
            };
            let ftext = text_of(f, src);
            match ftext.as_str() {
                "it" | "test" => {
                    if let Some(subject) = first_string_arg(node, src) {
                        push_tested_by(ir, &subject, parent.unwrap_or("module"), extractor, model);
                    }
                }
                "describe" => {
                    let dsubj = first_string_arg(node, src).unwrap_or_default();
                    if !dsubj.is_empty() {
                        push_tested_by(ir, &dsubj, parent.unwrap_or("module"), extractor, model);
                    }
                    // Nested it(/test( calls belong to the described unit.
                    let obj = if dsubj.is_empty() {
                        parent.unwrap_or("module").to_string()
                    } else {
                        dsubj
                    };
                    collect_ts_tests(node, src, ir, extractor, model, &obj);
                }
                _ => {}
            }
        }
        "export_statement" | "expression_statement" => {
            for c in named_children(node) {
                walk_ts_node(c, src, ir, extractor, model, parent);
            }
        }
        _ => {}
    }
}

/// Recursively find it(/test( calls inside a describe( body.
fn collect_ts_tests(
    node: Node,
    src: &str,
    ir: &mut KnowledgeIr,
    extractor: &str,
    model: &str,
    obj: &str,
) {
    for c in named_children(node) {
        if c.kind() != "call_expression" {
            collect_ts_tests(c, src, ir, extractor, model, obj);
            continue;
        }
        let Some(f) = c.child_by_field_name("function") else {
            continue;
        };
        match text_of(f, src).as_str() {
            "it" | "test" => {
                if let Some(subject) = first_string_arg(c, src) {
                    push_tested_by(ir, &subject, obj, extractor, model);
                }
            }
            "describe" => {
                let dsubj = first_string_arg(c, src).unwrap_or_default();
                let nested = if dsubj.is_empty() {
                    obj.to_string()
                } else {
                    dsubj
                };
                collect_ts_tests(c, src, ir, extractor, model, &nested);
            }
            _ => collect_ts_tests(c, src, ir, extractor, model, obj),
        }
    }
}

fn first_string_arg(node: Node, src: &str) -> Option<String> {
    let args = node.child_by_field_name("arguments")?;
    for a in named_children(args) {
        if a.kind() == "string" {
            let s = strip_ts_string(&text_of(a, src));
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// JSDoc block immediately above a node (comments are extras — siblings).
/// For exported declarations the comment sits above the `export_statement`
/// wrapper, so the doc anchor resolves to the outermost node.
fn ts_jsdoc(node: Node, src: &str) -> Vec<String> {
    let anchor = match node.parent() {
        Some(p) if p.kind() == "export_statement" => p,
        _ => node,
    };
    let mut lines = Vec::new();
    let mut sib = anchor.prev_sibling();
    while let Some(s) = sib {
        if s.kind() == "comment" {
            let text = text_of(s, src);
            if text.starts_with("/**") {
                for line in strip_jsdoc(&text) {
                    lines.push(line);
                }
            }
        }
        sib = s.prev_sibling();
    }
    lines.reverse();
    lines
}

fn strip_jsdoc(raw: &str) -> Vec<String> {
    let body = raw
        .strip_prefix("/**")
        .and_then(|s| s.strip_suffix("*/"))
        .unwrap_or(raw);
    body.lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn strip_ts_string(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 {
        let b = s.as_bytes();
        let (first, last) = (b[0], b[b.len() - 1]);
        if (first == b'"' && last == b'"')
            || (first == b'\'' && last == b'\'')
            || (first == b'`' && last == b'`')
        {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

fn walk_java(root: Node, src: &str, ir: &mut KnowledgeIr, extractor: &str, model: &str) {
    for node in named_children(root) {
        walk_java_node(node, src, ir, extractor, model, None);
    }
}

fn walk_java_node(
    node: Node,
    src: &str,
    ir: &mut KnowledgeIr,
    extractor: &str,
    model: &str,
    parent: Option<&str>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text_of(n, src))
                .unwrap_or_default();
            if name.is_empty() {
                return;
            }
            let full = match parent {
                Some(p) => format!("{}::{}", p, name),
                None => name.clone(),
            };
            let hint = if node.kind() == "class_declaration" {
                "Class"
            } else {
                "Interface"
            };
            let docs = java_javadoc(node, src);
            push_entity(ir, &full, hint, node, &docs, src, extractor, model);
            // extends/implements → IMPLEMENTS (superclass + interfaces fields).
            let mut targets = Vec::new();
            for field in ["superclass", "interfaces"] {
                if let Some(f) = node.child_by_field_name(field) {
                    collect_type_ids(f, src, &mut targets, 2);
                }
            }
            for t in targets {
                push_implements(ir, &full, &t, extractor, model);
            }
            if let Some(body) = node.child_by_field_name("body") {
                for b in named_children(body) {
                    walk_java_node(b, src, ir, extractor, model, Some(&full));
                }
            }
        }
        "method_declaration" | "constructor_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text_of(n, src))
                .unwrap_or_default();
            if name.is_empty() {
                return;
            }
            let full = match parent {
                Some(p) => format!("{}::{}", p, name),
                None => name.clone(),
            };
            let is_test =
                node.kind() == "method_declaration" && java_has_test_annotation(node, src);
            let hint = if is_test {
                "Test"
            } else if node.kind() == "constructor_declaration" {
                "Constructor"
            } else if parent.is_some() {
                "Method"
            } else {
                "Function"
            };
            let docs = java_javadoc(node, src);
            push_entity(ir, &full, hint, node, &docs, src, extractor, model);
            if is_test {
                push_tested_by(ir, &full, parent.unwrap_or("module"), extractor, model);
            }
        }
        "import_declaration" => {
            for c in named_children(node) {
                if c.kind() == "scoped_identifier" {
                    push_depends_on(
                        ir,
                        parent.unwrap_or("module"),
                        &text_of(c, src),
                        extractor,
                        model,
                    );
                }
            }
        }
        _ => {}
    }
}

/// Whether a method's modifiers carry a `@Test` marker annotation.
/// (`modifiers` is an unlabeled child in the java grammar, not a field.)
fn java_has_test_annotation(node: Node, src: &str) -> bool {
    for c in named_children(node) {
        if c.kind() != "modifiers" {
            continue;
        }
        for m in named_children(c) {
            if m.kind() == "marker_annotation" {
                if let Some(n) = m.child_by_field_name("name") {
                    if text_of(n, src) == "Test" {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Javadoc block immediately above a node (comments are extras — siblings).
/// The java grammar names its comments `block_comment`/`line_comment`, not
/// `comment` like TS.
fn java_javadoc(node: Node, src: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut sib = node.prev_sibling();
    while let Some(s) = sib {
        let kind = s.kind();
        if kind == "comment" || kind == "block_comment" || kind == "line_comment" {
            let text = text_of(s, src);
            if text.starts_with("/**") {
                for line in strip_jsdoc(&text) {
                    lines.push(line);
                }
            }
        }
        sib = s.prev_sibling();
    }
    lines.reverse();
    lines
}
