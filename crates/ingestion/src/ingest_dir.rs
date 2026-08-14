//! Directory ingestion — walk a directory tree, classify files, compile to
//! KnowledgeIr fragments, merge into one knowledge base. (MRFC-0070)

use crate::ir::*;
use crate::merge::merge_knowledge_ir;
use std::path::Path;
use std::process::Command;

/// Skip-list of directory names to never descend into.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "debug",
    "release",
    ".aikoql",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    ".turbo",
    "coverage",
    ".idea",
    ".vscode",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    "eggs",
    ".eggs",
    "wheelhouse",
];

/// Extensions that are always skipped (binaries, images, archives, compiled).
const SKIP_EXTENSIONS: &[&str] = &[
    "exe", "dll", "so", "dylib", "o", "a", "class", "pyc", "pyo", "png", "jpg", "jpeg", "gif",
    "bmp", "ico", "svg", "webp", "avif", "zip", "tar", "gz", "7z", "rar", "bz2", "xz", "zst",
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "mp3", "mp4", "avi", "mov", "mkv", "wav",
    "flac", "ttf", "otf", "woff", "woff2", "eot", "wasm", "bin", "dat", "db", "sqlite", "sqlite3",
];

/// Files to skip by exact basename (lockfiles, generated).
const SKIP_FILES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "composer.lock",
    "go.sum",
    "Cargo.toml.orig",
];

/// File extensions that map to language compilers.
const MARKDOWN_EXTS: &[&str] = &["md", "mdx"];
const RUST_EXTS: &[&str] = &["rs"];

/// Text-like extensions that get basic entity extraction (file → entity,
/// first doc-comment line → fact).
const TEXT_EXTS: &[&str] = &[
    "toml",
    "json",
    "yaml",
    "yml",
    "xml",
    "csv",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "py",
    "pyi",
    "go",
    "java",
    "kt",
    "kts",
    "c",
    "cpp",
    "cc",
    "cxx",
    "h",
    "hpp",
    "hh",
    "swift",
    "rb",
    "php",
    "pl",
    "pm",
    "sh",
    "bash",
    "zsh",
    "sql",
    "graphql",
    "proto",
    "css",
    "scss",
    "sass",
    "less",
    "html",
    "htm",
    "vue",
    "svelte",
    "astro",
    "txt",
    "log",
    "cfg",
    "ini",
    "conf",
    "env",
    "dockerfile",
    "makefile",
    "cmake",
    "tf",
    "tfvars",
    "hcl",
    "prisma",
    "nix",
    "lua",
    "r",
    "scala",
    "clj",
    "cljs",
    "edn",
    "elm",
    "ex",
    "exs",
    "erl",
    "hrl",
    "dart",
    "fs",
    "fsx",
    "groovy",
    "jl",
    "zig",
    "nim",
    "cr",
    "odin",
];

/// Result of directory ingestion — the merged IR plus statistics for reporting.
#[derive(Debug)]
pub struct IngestResult {
    pub ir: KnowledgeIr,
    pub files_processed: u32,
    pub files_skipped: u32,
    pub dirs_skipped: u32,
    pub binary_skipped: u32,
}

/// Ingest a directory tree into a single merged KnowledgeIr plus stats.
/// Returns the merged IR and statistics ready for storage and reporting.
pub fn ingest_directory(root: &str) -> Result<IngestResult, String> {
    let path = Path::new(root);
    if !path.is_dir() {
        return Err(format!("not a directory: {}", root));
    }

    let mut irs: Vec<KnowledgeIr> = Vec::new();
    let mut stats = IngestStats::default();
    walk(path, &mut irs, &mut stats)?;

    finalize_ingest_result(irs, stats, root)
}

/// Parallel version of ingest_directory: file discovery (sequential) →
/// compilation (rayon worker pool) → merge (sequential).
/// Throughput improves on multi-core for large repos. Memory is bounded by
/// the number of files; compilation is CPU-bound so num_cpus threads is
/// the natural bound.
pub fn parallel_ingest_directory(root: &str) -> Result<IngestResult, String> {
    let path = Path::new(root);
    if !path.is_dir() {
        return Err(format!("not a directory: {}", root));
    }

    // Phase 1: file discovery (sequential, fast)
    let mut file_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut stats = IngestStats::default();
    collect_file_paths(path, &mut file_paths, &mut stats)?;

    if file_paths.is_empty() {
        return Err("no source files found in directory".into());
    }

    // Phase 2: parallel compilation (CPU-bound, rayon worker pool)
    use rayon::prelude::*;
    let irs: Vec<KnowledgeIr> = file_paths
        .par_iter()
        .filter_map(|p| compile_file(p))
        .collect();

    finalize_ingest_result(irs, stats, root)
}

/// Shared result finalization for both sequential and parallel ingestion.
fn finalize_ingest_result(
    irs: Vec<KnowledgeIr>,
    stats: IngestStats,
    root: &str,
) -> Result<IngestResult, String> {
    if irs.is_empty() {
        return Err("no source files found in directory".into());
    }

    let mut merged = merge_knowledge_ir(&irs);
    merged.document_id = Some(format!("ingest-dir:{}", root));
    merged.extractor = "ingest-dir".into();
    merged.page_count = stats.files_processed;

    // Attach summary stats as facts
    merged.facts.push(FactCandidate {
        statement: format!(
            "Directory '{}' contained {} files ({} skipped: {} dirs, {} binary/lockfile). {} entities, {} relations, {} facts extracted.",
            root,
            stats.files_seen,
            stats.files_skipped,
            stats.dirs_skipped,
            stats.binary_skipped,
            merged.entities.len(),
            merged.relations.len(),
            merged.facts.len(),
        ),
        entities: vec![],
        confidence: 1.0,
        evidence: Evidence {
            document_id: merged.document_id.clone(),
            page: None,
            bbox_text: None,
            extractor: "ingest-dir".into(),
            model: None,
            confidence: 1.0,
        },
    });

    Ok(IngestResult {
        ir: merged,
        files_processed: stats.files_processed,
        files_skipped: stats.files_skipped,
        dirs_skipped: stats.dirs_skipped,
        binary_skipped: stats.binary_skipped,
    })
}

#[derive(Default)]
pub struct IngestStats {
    files_seen: u32,
    files_processed: u32,
    files_skipped: u32,
    dirs_skipped: u32,
    binary_skipped: u32,
}

fn walk(dir: &Path, irs: &mut Vec<KnowledgeIr>, stats: &mut IngestStats) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => return Err(format!("read_dir {}: {}", dir.display(), e)),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if path.is_dir() {
            if SKIP_DIRS.contains(&fname.to_lowercase().as_str()) || fname.starts_with('.') {
                stats.dirs_skipped += 1;
                continue;
            }
            walk(&path, irs, stats)?;
            continue;
        }

        stats.files_seen += 1;

        // Skip-list checks before compilation
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let basename = fname.to_lowercase();

        if SKIP_EXTENSIONS.contains(&ext.as_str()) {
            stats.binary_skipped += 1;
            stats.files_skipped += 1;
            continue;
        }
        if SKIP_FILES.iter().any(|s| s.to_lowercase() == basename) {
            stats.files_skipped += 1;
            continue;
        }
        if is_binary_file(&path) {
            stats.binary_skipped += 1;
            stats.files_skipped += 1;
            continue;
        }

        if let Some(ir) = compile_file(&path) {
            stats.files_processed += 1;
            irs.push(ir);
        } else {
            stats.binary_skipped += 1;
            stats.files_skipped += 1;
        }
    }

    Ok(())
}

/// Walk directory tree collecting file paths only (no compilation).
/// Same skip logic as `walk()` — skips .git, node_modules, binaries, etc.
pub fn collect_file_paths(
    dir: &Path,
    paths: &mut Vec<std::path::PathBuf>,
    stats: &mut IngestStats,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => return Err(format!("read_dir {}: {}", dir.display(), e)),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if path.is_dir() {
            if SKIP_DIRS.contains(&fname.to_lowercase().as_str()) || fname.starts_with('.') {
                stats.dirs_skipped += 1;
                continue;
            }
            collect_file_paths(&path, paths, stats)?;
            continue;
        }

        stats.files_seen += 1;

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let basename = fname.to_lowercase();

        if SKIP_EXTENSIONS.contains(&ext.as_str()) {
            stats.binary_skipped += 1;
            stats.files_skipped += 1;
            continue;
        }
        if SKIP_FILES.iter().any(|s| s.to_lowercase() == basename) {
            stats.files_skipped += 1;
            continue;
        }
        if is_binary_file(&path) {
            stats.binary_skipped += 1;
            stats.files_skipped += 1;
            continue;
        }

        stats.files_processed += 1;
        paths.push(path);
    }

    Ok(())
}

/// Compile a single file to KnowledgeIr (or None if unclassifiable).
/// Extracted from `walk()` so parallel and incremental ingestion can
/// call it independently without the tree-walk coupling.
pub fn compile_file(path: &Path) -> Option<KnowledgeIr> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let fname = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    if MARKDOWN_EXTS.contains(&ext.as_str()) {
        crate::markdown::compile_markdown_file(
            &path.to_string_lossy(),
            Some(path.to_string_lossy().to_string()),
        )
        .ok()
        .filter(|ir| !ir.entities.is_empty())
        .or_else(|| Some(file_as_entity(path)))
    } else if RUST_EXTS.contains(&ext.as_str()) {
        crate::code::compile_rust_file(&path.to_string_lossy())
            .ok()
            .filter(|ir| !ir.entities.is_empty())
            .or_else(|| Some(file_as_entity(path)))
    } else if TEXT_EXTS.contains(&ext.as_str()) {
        Some(text_file_ir(path))
    } else if ext.is_empty() {
        let lower = fname.to_lowercase();
        if lower == "dockerfile"
            || lower == "makefile"
            || lower == "justfile"
            || lower == "license"
            || lower.starts_with("dockerfile.")
        {
            Some(text_file_ir(path))
        } else if is_binary_file(path) {
            None
        } else {
            Some(text_file_ir(path))
        }
    } else {
        Some(text_file_ir(path))
    }
}

/// Check if a file is binary by looking for null bytes in the first 512 bytes.
fn is_binary_file(path: &Path) -> bool {
    let Ok(data) = std::fs::read(path) else {
        return true;
    };
    if data.is_empty() {
        return false;
    }
    data.iter().take(512).any(|&b| b == 0)
}

/// Minimal IR: file path as entity, no facts.
fn file_as_entity(path: &Path) -> KnowledgeIr {
    let name = path.to_string_lossy().to_string();
    KnowledgeIr {
        entities: vec![EntityCandidate {
            name: name.clone(),
            type_hint: Some("file".into()),
            mentions: vec![name.clone()],
            confidence: 1.0,
            evidence: file_evidence(&name),
        }],
        relations: vec![],
        facts: vec![],
        events: vec![],
        temporal: vec![],
        document_id: Some(name),
        page_count: 1,
        extractor: "ingest-dir".into(),
    }
}

/// Build IR from a generic text file: file as entity + first meaningful line as fact.
fn text_file_ir(path: &Path) -> KnowledgeIr {
    let name = path.to_string_lossy().to_string();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let type_hint = file_type_hint(&ext, path);

    let mut facts = Vec::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        // Grab the first non-empty, non-shebang line as a descriptive fact
        for line in content.lines().take(20) {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("#!") || trimmed.starts_with("<?xml") {
                continue;
            }
            let fact_line = if trimmed.len() > 300 {
                format!("{}...", &trimmed[..300])
            } else {
                trimmed.to_string()
            };
            facts.push(FactCandidate {
                statement: format!("File '{}': {}", name, fact_line),
                entities: vec![name.clone()],
                confidence: 0.5,
                evidence: file_evidence(&name),
            });
            break;
        }
    }

    KnowledgeIr {
        entities: vec![EntityCandidate {
            name: name.clone(),
            type_hint: Some(type_hint),
            mentions: vec![name.clone()],
            confidence: 1.0,
            evidence: file_evidence(&name),
        }],
        relations: vec![],
        facts,
        events: vec![],
        temporal: vec![],
        document_id: Some(name),
        page_count: 1,
        extractor: "ingest-dir".into(),
    }
}

fn file_type_hint(ext: &str, path: &Path) -> String {
    match ext {
        "toml" | "yaml" | "yml" | "json" | "xml" | "ini" | "cfg" | "conf" | "env" | "hcl"
        | "tfvars" => "config".into(),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => "typescript".into(),
        "py" | "pyi" => "python".into(),
        "go" => "go".into(),
        "java" | "kt" | "kts" | "scala" | "groovy" => "jvm".into(),
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "hh" => "c-family".into(),
        "swift" => "swift".into(),
        "rb" => "ruby".into(),
        "php" => "php".into(),
        "sh" | "bash" | "zsh" => "shell".into(),
        "sql" => "sql".into(),
        "css" | "scss" | "sass" | "less" => "stylesheet".into(),
        "html" | "htm" | "vue" | "svelte" | "astro" => "markup".into(),
        "proto" | "graphql" => "schema".into(),
        "tf" => "terraform".into(),
        "prisma" => "prisma".into(),
        "dockerfile" | "makefile" | "cmake" => "build".into(),
        "lua" => "lua".into(),
        "r" => "r".into(),
        "rs" => "rust".into(),
        "md" | "mdx" => "documentation".into(),
        _ => {
            let fname = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();
            if fname.starts_with("dockerfile") || fname == "makefile" || fname == "justfile" {
                "build".into()
            } else {
                "file".into()
            }
        }
    }
}

fn file_evidence(name: &str) -> Evidence {
    Evidence {
        document_id: Some(name.to_string()),
        page: None,
        bbox_text: None,
        extractor: "ingest-dir".into(),
        model: None,
        confidence: 1.0,
    }
}

/// Summary statistics gathered during directory ingestion.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct IngestReport {
    pub repo_name: String,
    pub revision: String,
    pub path: String,
    pub files_processed: u32,
    pub files_skipped: u32,
    pub dirs_skipped: u32,
    pub binary_skipped: u32,
    pub entities: usize,
    pub relations: usize,
    pub facts: usize,
    pub events: usize,
    pub components: usize,
    pub tests: usize,
    pub programs: usize,
    pub documents: usize,
    pub ontology_entities: usize,
    pub ontology_relations: usize,
    pub provenance_pct: f64,
    pub resolved_pct: f64,
    pub unresolved: usize,
    pub conflicts: usize,
    pub stale: usize,
}

/// Compute an ingest report from a merged KnowledgeIr.
pub fn build_report(
    ir: &KnowledgeIr,
    path: &str,
    files_processed: u32,
    files_skipped: u32,
    dirs_skipped: u32,
    binary_skipped: u32,
) -> IngestReport {
    let (repo_name, revision) = git_info(path);

    let components = ir
        .entities
        .iter()
        .filter(|e| {
            e.type_hint.as_deref() == Some("component")
                || e.type_hint.as_deref() == Some("module")
                || e.type_hint.as_deref() == Some("service")
        })
        .count();
    let tests = ir
        .entities
        .iter()
        .filter(|e| {
            e.type_hint.as_deref() == Some("test")
                || e.name.contains("test")
                || e.name.ends_with("_test")
        })
        .count();
    let programs = ir
        .entities
        .iter()
        .filter(|e| {
            e.type_hint.as_deref() == Some("program") || e.type_hint.as_deref() == Some("script")
        })
        .count();
    let documents = ir
        .entities
        .iter()
        .filter(|e| {
            e.type_hint.as_deref() == Some("documentation")
                || e.type_hint.as_deref() == Some("document")
        })
        .count();

    // Ontology: count unique type_hints as ontology entities, unique predicates as relationship types
    let mut type_hints: Vec<&str> = ir
        .entities
        .iter()
        .filter_map(|e| e.type_hint.as_deref())
        .collect();
    type_hints.sort();
    type_hints.dedup();
    let mut predicates: Vec<&str> = ir.relations.iter().map(|r| r.predicate.as_str()).collect();
    predicates.sort();
    predicates.dedup();

    // Provenance: % of entities with non-empty evidence
    let total_entities = ir.entities.len().max(1);
    let with_evidence = ir
        .entities
        .iter()
        .filter(|e| e.evidence.document_id.is_some())
        .count();
    let provenance_pct = (with_evidence as f64 / total_entities as f64) * 100.0;

    // Resolved: % of entities that appear in at least one relation (connected)
    let connected: std::collections::HashSet<&str> = ir
        .relations
        .iter()
        .flat_map(|r| [r.subject.as_str(), r.object.as_str()])
        .collect();
    let resolved = ir
        .entities
        .iter()
        .filter(|e| connected.contains(e.name.as_str()))
        .count();
    let resolved_pct = (resolved as f64 / total_entities as f64) * 100.0;
    let unresolved = total_entities - resolved;

    // Potential conflicts: facts about the same entity with contradictory patterns
    let conflicts = crate::staleness::detect_staleness(&ir.facts, &[])
        .len()
        .min(999);

    IngestReport {
        repo_name,
        revision,
        path: path.to_string(),
        files_processed,
        files_skipped: files_skipped + binary_skipped,
        dirs_skipped,
        binary_skipped,
        entities: ir.entities.len(),
        relations: ir.relations.len(),
        facts: ir.facts.len(),
        events: ir.events.len(),
        components,
        tests,
        programs,
        documents,
        ontology_entities: type_hints.len(),
        ontology_relations: predicates.len(),
        provenance_pct,
        resolved_pct,
        unresolved,
        conflicts,
        stale: 0, // staleness needs a baseline to compare against
    }
}

/// Format a knowledge report as a boxed table.
pub fn format_report(report: &IngestReport) -> String {
    let w = 48; // box width
    let hr = "─".repeat(w);
    let title = "Aikoql KNOWLEDGE REPORT";
    let pad = (w - title.len()) / 2;
    let pad_r = w - title.len() - pad;

    let kv = |k: &str, v: &dyn std::fmt::Display| format!("  {:<24} {:>20}", format!("{}:", k), v);

    let mut lines = vec![
        format!("╔{}╗", hr),
        format!("║{}{}{}║", " ".repeat(pad), title, " ".repeat(pad_r)),
        format!("╚{}╝", hr),
        String::new(),
    ];

    if !report.repo_name.is_empty() {
        lines.push(kv("Repository", &report.repo_name));
    }
    if !report.revision.is_empty() {
        lines.push(kv("Revision", &report.revision));
    }
    lines.push(kv("Path", &report.path));
    lines.push(String::new());
    lines.push(format!("  {:-<24} {:-<22}", "", ""));
    lines.push(format!(
        "  {:<24} {:>20}",
        "Files processed:", report.files_processed
    ));
    lines.push(format!(
        "  {:<24} {:>20}",
        "Files skipped:", report.files_skipped
    ));
    lines.push(format!(
        "  {:<24} {:>20}",
        "Directories skipped:", report.dirs_skipped
    ));
    lines.push(String::new());
    lines.push(format!("  {:<24} {:>20}", "Entities:", report.entities));
    lines.push(format!("  {:<24} {:>20}", "Relations:", report.relations));
    lines.push(format!("  {:<24} {:>20}", "Facts:", report.facts));
    lines.push(String::new());
    lines.push(format!("  {:<24} {:>20}", "Components:", report.components));
    lines.push(format!("  {:<24} {:>20}", "Tests:", report.tests));
    lines.push(format!("  {:<24} {:>20}", "Programs:", report.programs));
    lines.push(format!("  {:<24} {:>20}", "Documents:", report.documents));
    lines.push(String::new());
    lines.push("  Ontology:".to_string());
    lines.push(format!(
        "    {:<22} {:>20}",
        "Entity types:", report.ontology_entities
    ));
    lines.push(format!(
        "    {:<22} {:>20}",
        "Relationship types:", report.ontology_relations
    ));
    lines.push(String::new());
    lines.push("  Knowledge quality:".to_string());
    lines.push(format!(
        "    {:<22} {:>19.1}%",
        "Provenance:", report.provenance_pct
    ));
    lines.push(format!(
        "    {:<22} {:>19.1}%",
        "Resolved entities:", report.resolved_pct
    ));
    lines.push(format!(
        "    {:<22} {:>20}",
        "Unresolved:", report.unresolved
    ));
    lines.push(String::new());
    if report.conflicts > 0 {
        lines.push("  Potential conflicts:".to_string());
        lines.push(format!("    {:>44}", report.conflicts));
        lines.push(String::new());
    }
    if report.stale > 0 {
        lines.push("  Stale knowledge:".to_string());
        lines.push(format!("    {:>44}", report.stale));
    }

    lines.join("\n")
}

fn git_info(path: &str) -> (String, String) {
    let repo_name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let revision = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(path)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    (repo_name, revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn ingest_temp_dir_mixed() {
        let tmp = std::env::temp_dir().join("aikoql-ingest-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::create_dir_all(tmp.join("docs")).unwrap();
        fs::create_dir_all(tmp.join("node_modules")).unwrap(); // should be skipped
        fs::create_dir_all(tmp.join(".git")).unwrap(); // should be skipped

        // Write a markdown file
        fs::write(
            tmp.join("README.md"),
            "# Architecture\n\nThe system uses a pipeline.\n",
        )
        .unwrap();
        // Write a Rust file
        fs::write(
            tmp.join("src/main.rs"),
            "/// Main entry point.\npub fn main() {}\n",
        )
        .unwrap();
        // Write a TOML config
        fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        // Write a file in skipped dir
        fs::write(tmp.join("node_modules/pkg.js"), "module.exports = {};").unwrap();
        // Write a binary file
        let mut bin = std::fs::File::create(tmp.join("icon.png")).unwrap();
        bin.write_all(&[0x89, b'P', b'N', b'G', 0, 0, 0]).unwrap();

        let result = ingest_directory(&tmp.to_string_lossy()).expect("ingest");
        let ir = &result.ir;

        // Should have entities: README.md, src/main.rs, Cargo.toml
        // plus any entities extracted from markdown content (Architecture heading)
        assert!(
            ir.entities.len() >= 3,
            "expected at least 3 entities, got {}",
            ir.entities.len()
        );
        // node_modules and .git and icon.png should NOT appear
        for ent in &ir.entities {
            assert!(
                !ent.name.contains("node_modules"),
                "node_modules should be skipped: {}",
                ent.name
            );
            assert!(
                !ent.name.contains(".git"),
                ".git should be skipped: {}",
                ent.name
            );
            assert!(
                !ent.name.contains("png"),
                "png should be skipped: {}",
                ent.name
            );
        }
        // Should have facts from markdown and Cargo.toml
        assert!(!ir.facts.is_empty(), "expected facts");
        // document_id should be set
        assert!(ir.document_id.is_some());
        // Stats should be populated
        assert!(result.files_processed >= 3);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn compile_file_empty_ir_falls_back_to_path_entity() {
        let tmp = std::env::temp_dir().join("aikoql-empty-ir-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Macro-only rust: parses fine, but zero extractable items.
        let rs = tmp.join("fuzz.rs");
        fs::write(
            &rs,
            "proptest! {\n#![proptest_config(std::env::var(\"x\").ok())]\n}\n",
        )
        .unwrap();
        // Markdown with frontmatter but no recognized sections (the shape that
        // made website/docs/sdk/*.md vanish from the KB).
        let md = tmp.join("notes.md");
        fs::write(
            &md,
            "---\ntitle: Go SDK\ndescription: A client\n---\n# Go SDK\n\nA client.\n",
        )
        .unwrap();

        for f in [&rs, &md] {
            let ir = compile_file(f).expect("fallback must produce an IR");
            assert!(!ir.entities.is_empty(), "{:?} vanished", f);
        }
        // Macro-only rust extracts zero items, so the path entity must appear.
        let ir = compile_file(&rs).expect("fallback must produce an IR");
        assert_eq!(ir.entities[0].name, rs.to_string_lossy());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_dir_errors() {
        let tmp = std::env::temp_dir().join("aikoql-ingest-empty");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let result = ingest_directory(&tmp.to_string_lossy());
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn non_existent_dir_errors() {
        let result = ingest_directory("/nonexistent/path/12345");
        assert!(result.is_err());
    }

    #[test]
    fn binary_detection() {
        let tmp = std::env::temp_dir().join("aikoql-bin-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // text file
        fs::write(tmp.join("hello.txt"), "hello world").unwrap();
        assert!(!is_binary_file(&tmp.join("hello.txt")));
        // binary file
        fs::write(tmp.join("data.bin"), &[0u8, 1, 2, 3]).unwrap();
        assert!(is_binary_file(&tmp.join("data.bin")));
        let _ = fs::remove_dir_all(&tmp);
    }
}
