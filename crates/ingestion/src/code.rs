//! MRFC-0070 Phase A2: Code-to-Knowledge Compiler — Rust.
//!
//! Parses Rust source files via `syn` and produces typed KnowledgeIr:
//! modules, structs, enums, traits, functions, impls → entities,
//! `use` imports → DEPENDS_ON, `#[test]` → TESTED_BY,
//! doc comments (`///`, `//!`) → Claim facts.

use crate::ir::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};
use mnemosyne_kernel::knowledge::kom;

/// Compile a Rust source file into KnowledgeIr.
///
/// On parse failure, returns an empty IR with a warning logged to stderr.
pub fn compile_rust_file(path: &str) -> Result<KnowledgeIr, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("read '{}': {}", path, e))?;
    Ok(compile_rust_source(&src, Some(path)))
}

/// Compile a Rust source string into KnowledgeIr.
pub fn compile_rust_source(src: &str, file_hint: Option<&str>) -> KnowledgeIr {
    let document_id = file_hint.map(|s| s.to_string());
    let extractor = "rust-code-parser".to_string();

    match syn::parse_file(src) {
        Ok(file) => parse_syn_file(&file, document_id, extractor),
        Err(e) => {
            eprintln!("rust-code-parser: syn parse error: {}", e);
            KnowledgeIr {
                document_id,
                extractor: "rust-code-parser".into(),
                ..Default::default()
            }
        }
    }
}

fn parse_syn_file(file: &syn::File, document_id: Option<String>, extractor: String) -> KnowledgeIr {
    let mut ir = KnowledgeIr {
        document_id,
        extractor,
        ..Default::default()
    };

    // Module-level doc comment (#![…] or //!)
    for attr in &file.attrs {
        if let Some(doc) = extract_doc_comment(attr) {
            ir.facts.push(FactCandidate {
                statement: doc,
                entities: vec!["crate".into()],
                confidence: 0.8,
                evidence: module_evidence(file, "crate-doc"),
            });
        }
    }

    for item in &file.items {
        process_item(item, &mut ir, "crate");
    }

    ir
}

fn process_item(item: &syn::Item, ir: &mut KnowledgeIr, parent: &str) {
    let extractor = "rust-code-parser".to_string();

    match item {
        syn::Item::Mod(m) => {
            let name = m.ident.to_string();
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some("Module".into()),
                mentions: doc_lines(&m.attrs),
                confidence: 0.85,
                evidence: span_evidence(&extractor, &name, "module"),
            });
            // Process module content if inline
            if let Some((_, items)) = &m.content {
                for inner in items {
                    process_item(inner, ir, &name);
                }
            }
        }

        syn::Item::Struct(s) => {
            let name = s.ident.to_string();
            let docs = doc_lines(&s.attrs);
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some("Struct".into()),
                mentions: docs.clone(),
                confidence: 0.85,
                evidence: span_evidence(&extractor, &name, "struct"),
            });
            for doc in &docs {
                ir.facts.push(FactCandidate {
                    statement: doc.clone(),
                    entities: vec![name.clone()],
                    confidence: 0.75,
                    evidence: span_evidence(&extractor, &name, "doc"),
                });
            }
        }

        syn::Item::Enum(e) => {
            let name = e.ident.to_string();
            let docs = doc_lines(&e.attrs);
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some("Enum".into()),
                mentions: docs.clone(),
                confidence: 0.85,
                evidence: span_evidence(&extractor, &name, "enum"),
            });
            for doc in &docs {
                ir.facts.push(FactCandidate {
                    statement: doc.clone(),
                    entities: vec![name.clone()],
                    confidence: 0.75,
                    evidence: span_evidence(&extractor, &name, "doc"),
                });
            }
        }

        syn::Item::Trait(tr) => {
            let name = tr.ident.to_string();
            let docs = doc_lines(&tr.attrs);
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some("Trait".into()),
                mentions: docs.clone(),
                confidence: 0.85,
                evidence: span_evidence(&extractor, &name, "trait"),
            });
            for doc in &docs {
                ir.facts.push(FactCandidate {
                    statement: doc.clone(),
                    entities: vec![name.clone()],
                    confidence: 0.75,
                    evidence: span_evidence(&extractor, &name, "doc"),
                });
            }
        }

        syn::Item::Fn(f) => {
            let name = f.sig.ident.to_string();
            let docs = doc_lines(&f.attrs);
            let is_test = f.attrs.iter().any(|a| a.path().is_ident("test"));

            let type_hint = if is_test { "Test" } else { "Function" };
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some(type_hint.into()),
                mentions: docs.clone(),
                confidence: 0.85,
                evidence: span_evidence(&extractor, &name, "fn"),
            });

            for doc in &docs {
                ir.facts.push(FactCandidate {
                    statement: doc.clone(),
                    entities: vec![name.clone()],
                    confidence: 0.75,
                    evidence: span_evidence(&extractor, &name, "doc"),
                });
            }

            // #[test] functions → TESTED_BY relationship
            if is_test {
                ir.relations.push(RelationCandidate {
                    subject: name.clone(),
                    predicate: kom::TESTED_BY.to_string(),
                    object: parent.to_string(),
                    confidence: 0.8,
                    evidence: span_evidence(&extractor, &name, "test-attr"),
                });
            }
        }

        syn::Item::Impl(imp) => {
            let name = type_or_trait_name(imp);
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some("Impl".into()),
                mentions: doc_lines(&imp.attrs),
                confidence: 0.85,
                evidence: span_evidence(&extractor, &name, "impl"),
            });

            // impl blocks often imply IMPLEMENTS relationship
            if let Some((_, trait_path, _)) = &imp.trait_ {
                let trait_name = path_to_string(trait_path);
                ir.relations.push(RelationCandidate {
                    subject: name.clone(),
                    predicate: kom::IMPLEMENTS.to_string(),
                    object: trait_name,
                    confidence: 0.8,
                    evidence: span_evidence(&extractor, &name, "impl-trait"),
                });
            }

            for inner in &imp.items {
                if let syn::ImplItem::Fn(m) = inner {
                    let method_name = m.sig.ident.to_string();
                    let full = format!("{}::{}", name, method_name);
                    let docs = doc_lines(&m.attrs);
                    ir.entities.push(EntityCandidate {
                        name: full.clone(),
                        type_hint: Some("Method".into()),
                        mentions: docs.clone(),
                        confidence: 0.85,
                        evidence: span_evidence(&extractor, &full, "method"),
                    });
                    for doc in &docs {
                        ir.facts.push(FactCandidate {
                            statement: doc.clone(),
                            entities: vec![full.clone()],
                            confidence: 0.75,
                            evidence: span_evidence(&extractor, &full, "doc"),
                        });
                    }
                }
            }
        }

        syn::Item::Use(u) => {
            let target = use_tree_to_string(&u.tree);
            ir.relations.push(RelationCandidate {
                subject: parent.to_string(),
                predicate: kom::DEPENDS_ON.to_string(),
                object: target,
                confidence: 0.85,
                evidence: span_evidence(&extractor, parent, "use"),
            });
        }

        syn::Item::Const(c) => {
            let name = c.ident.to_string();
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some("Constant".into()),
                mentions: doc_lines(&c.attrs),
                confidence: 0.80,
                evidence: span_evidence(&extractor, &name, "const"),
            });
        }

        syn::Item::Type(ty) => {
            let name = ty.ident.to_string();
            ir.entities.push(EntityCandidate {
                name: name.clone(),
                type_hint: Some("TypeAlias".into()),
                mentions: doc_lines(&ty.attrs),
                confidence: 0.80,
                evidence: span_evidence(&extractor, &name, "type"),
            });
        }

        // Macro invocations at module level — skip
        syn::Item::Macro(_) => {}

        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_doc_comment(attr: &syn::Attribute) -> Option<String> {
    if !attr.path().is_ident("doc") {
        return None;
    }
    // syn stores doc comments as #[doc = "text"] or #[doc = r"text"]
    if let syn::Meta::NameValue(nv) = &attr.meta {
        if let syn::Expr::Lit(lit) = &nv.value {
            if let syn::Lit::Str(s) = &lit.lit {
                let val = s.value();
                let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

fn doc_lines(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs.iter().filter_map(extract_doc_comment).collect()
}

fn span_evidence(extractor: &str, name: &str, kind: &str) -> Evidence {
    Evidence {
        document_id: None,
        page: None,
        bbox_text: Some(format!("{}: {} {}", kind, extractor, name)),
        extractor: extractor.to_string(),
        model: Some("syn-v2".into()),
        confidence: 0.85,
    }
}

fn module_evidence(_file: &syn::File, kind: &str) -> Evidence {
    Evidence {
        document_id: None,
        page: None,
        bbox_text: Some(kind.to_string()),
        extractor: "rust-code-parser".into(),
        model: Some("syn-v2".into()),
        confidence: 0.85,
    }
}

fn path_to_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn type_or_trait_name(imp: &syn::ItemImpl) -> String {
    // impl TraitName for TypeName { ... } or impl TypeName { ... }
    if let Some((_, trait_path, _)) = &imp.trait_ {
        format!("impl {} for {}", path_to_string(trait_path), type_path_to_string(&imp.self_ty))
    } else {
        format!("impl {}", type_path_to_string(&imp.self_ty))
    }
}

fn type_path_to_string(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(tp) => path_to_string(&tp.path),
        syn::Type::Reference(r) => type_path_to_string(&r.elem),
        other => format!("{:?}", other),
    }
}

fn use_tree_to_string(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(p) => {
            let prefix = p.ident.to_string();
            format!("{}::{}", prefix, use_tree_to_string(&p.tree))
        }
        syn::UseTree::Name(n) => n.ident.to_string(),
        syn::UseTree::Rename(r) => format!("{} as {}", r.ident, r.rename),
        syn::UseTree::Glob(_) => "*".to_string(),
        syn::UseTree::Group(g) => g
            .items
            .iter()
            .map(use_tree_to_string)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_struct_with_doc() {
        let src = r#"
/// A constraint engine that validates rules.
struct ConstraintEngine {
    rules: Vec<String>,
}
"#;
        let ir = compile_rust_source(src, Some("test.rs"));
        let entity = ir
            .entities
            .iter()
            .find(|e| e.name == "ConstraintEngine")
            .expect("should find ConstraintEngine struct");
        assert_eq!(entity.type_hint.as_deref(), Some("Struct"));
        assert!(!entity.mentions.is_empty(), "should have doc comment");
        assert!(ir
            .facts
            .iter()
            .any(|f| f.statement.contains("constraint engine")),
            "should have doc fact");
    }

    #[test]
    fn parse_fn_with_test_attr() {
        let src = r#"
#[test]
fn test_constraint_validation() {
    assert!(true);
}
"#;
        let ir = compile_rust_source(src, Some("test.rs"));
        let entity = ir
            .entities
            .iter()
            .find(|e| e.name == "test_constraint_validation")
            .expect("should find test function");
        assert_eq!(entity.type_hint.as_deref(), Some("Test"));
        assert!(ir
            .relations
            .iter()
            .any(|r| r.predicate == kom::TESTED_BY),
            "should have TESTED_BY relation");
    }

    #[test]
    fn parse_use_creates_depends_on() {
        let src = r#"use std::collections::HashMap;"#;
        let ir = compile_rust_source(src, Some("test.rs"));
        assert!(ir
            .relations
            .iter()
            .any(|r| r.predicate == kom::DEPENDS_ON),
            "use should create DEPENDS_ON");
    }

    #[test]
    fn parse_impl_trait_creates_implements() {
        let src = r#"
trait Validator { fn validate(&self); }
struct MyValidator;
impl Validator for MyValidator {
    fn validate(&self) {}
}
"#;
        let ir = compile_rust_source(src, Some("test.rs"));
        assert!(ir
            .entities
            .iter()
            .any(|e| e.type_hint.as_deref() == Some("Trait")),
            "should find Validator trait");
        assert!(ir
            .entities
            .iter()
            .any(|e| e.type_hint.as_deref() == Some("Impl")),
            "should find impl");
        assert!(ir
            .relations
            .iter()
            .any(|r| r.predicate == kom::IMPLEMENTS),
            "should have IMPLEMENTS relation");
    }

    #[test]
    fn parse_inline_module() {
        let src = r#"
mod engine {
    /// The constraint module.
    pub struct Constraint;
}
"#;
        let ir = compile_rust_source(src, Some("test.rs"));
        assert!(ir
            .entities
            .iter()
            .any(|e| e.name == "engine" && e.type_hint.as_deref() == Some("Module")),
            "should find module");
        assert!(ir
            .entities
            .iter()
            .any(|e| e.name == "Constraint"),
            "should find struct inside module");
    }

    #[test]
    fn parse_error_is_graceful() {
        // Invalid Rust — syn should fail
        let src = "not rust code at all ###!!!";
        let ir = compile_rust_source(src, Some("test.rs"));
        // Should return empty IR, not panic
        assert_eq!(ir.entities.len(), 0);
        assert_eq!(ir.facts.len(), 0);
    }

    #[test]
    fn parse_enum_with_variants() {
        let src = r#"
/// Lifecycle states for knowledge objects.
enum LifecycleState {
    Draft,
    Active,
    Verified,
}
"#;
        let ir = compile_rust_source(src, Some("test.rs"));
        assert!(ir
            .entities
            .iter()
            .any(|e| e.name == "LifecycleState" && e.type_hint.as_deref() == Some("Enum")),
            "should find enum");
    }
}
