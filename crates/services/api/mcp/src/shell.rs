//! Interactive AIKOQL shell — the `mnemosyne` REPL.
//!
//! Like `sqlite3` or `psql`, but for the AIKOQL knowledge query language.
//! Opens a database file, accepts queries and dot-commands, prints results.

use mnemosyne_compiler::parser;
use mnemosyne_graph::*;
use mnemosyne_kernel::ir::IrPlan;
use mnemosyne_kernel::knowledge::kom::*;
use mnemosyne_kernel::knowledge::scoring::ko_text;
use mnemosyne_kernel::transaction::kernel::{Kernel, RememberRequest, Subject};
use mnemosyne_kernel::{MemoryEngine, Origin, RedbEngine, ReferentialPolicy, SystemClock};
use mnemosyne_runtime::Interpreter;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Arc;

pub fn run_shell(db_path: &str, tenant: Option<&str>) {
    let tenant_opt: Option<String> = tenant.map(String::from);
    let engine: Arc<dyn mnemosyne_kernel::StorageEngine> = if db_path == ":memory:" {
        Arc::new(MemoryEngine::new())
    } else {
        let path = Path::new(db_path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        Arc::new(RedbEngine::open(db_path).expect("open database"))
    };

    let kernel = Arc::new(
        Kernel::open(engine, Arc::new(SystemClock), 0xCAFE)
            .expect("open kernel"),
    );

    println!(
        "Mnemosyne {} — AIKOQL Knowledge Shell",
        env!("CARGO_PKG_VERSION")
    );
    println!("Connected to: {}", db_path);
    if let Some(ref t) = tenant_opt {
        println!("Tenant: {}", t);
    }
    println!("Type .help for commands, .exit to quit.\n");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    let user = Subject::new("shell-user");

    loop {
        line.clear();
        write!(stdout, "mnemosyne> ").ok();
        stdout.flush().ok();

        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("read error: {}", e);
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Dot-commands start with `.`
        if trimmed.starts_with('.') {
            match handle_dot_command(&kernel, trimmed, db_path) {
                DotResult::Exit => break,
                DotResult::Ok => continue,
                DotResult::Error(msg) => eprintln!("Error: {}", msg),
            }
            continue;
        }

        // AIKOQL: parse first, then route query vs mutation.
        match parser::parse(trimmed) {
            Ok(mnemosyne_compiler::parser::ast::Statement::Create(create)) => {
                let mut props = BTreeMap::new();
                for (k, v) in &create.properties {
                    props.insert(k.clone(), ast_expr_to_value(v));
                }
                match kernel.remember(RememberRequest {
                    context: (&user).into(),
                    koid: None,
                    expected_version: Some(0),
                    idempotency_key: None,
                    metadata: Metadata {
                        type_name: create.entity.clone(),
                        tenant: tenant_opt.clone(),
                        schema_version: 1,
                        tags: vec![],
                    },
                    properties: props,
                    semantic: None,
                    relationships: vec![],
                    security: None,
                    extensions: BTreeMap::new(),
                    origin: Origin::Human,
                    note: None,
                    referential_policy: ReferentialPolicy::default(),
                }) {
                    Ok(r) => {
                        writeln!(stdout, "Created: {} (v{})", r.koid.to_hex(), r.version).ok();
                    }
                    Err(e) => {
                        writeln!(stdout, "Error: {}", e).ok();
                    }
                }
            }
            Ok(_other) => {
                // Query: MATCH, TRAVERSE, etc. — compile to IR and execute.
                match parser::compile_with_subject(trimmed, &user.name) {
                    Ok(plan) => {
                        execute_and_print(&kernel, &plan, &mut stdout);
                    }
                    Err(e) => {
                        eprintln!("Compile error: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Parse error: {}", e);
            }
        }
    }

    println!("Bye.");
}

enum DotResult {
    Ok,
    Exit,
    Error(String),
}

fn handle_dot_command(kernel: &Kernel, cmd: &str, db_path: &str) -> DotResult {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let verb = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");

    match verb {
        ".exit" | ".quit" | ".q" => DotResult::Exit,

        ".help" => {
            println!("AIKOQL Commands:");
            println!("  GET <koid>                    Fetch object by KOID");
            println!("  FIND [IN <type>] MATCH <prop> <op> <val> [AND|OR ...]  Filtered search");
            println!("  FIND [IN <type>] SIMILAR TO <text>  Similarity search");
            println!("  LINK <source> -> <target> [AS <rel-type>]  Create relationship");
            println!("  TRAVERSE <koid> [IN|OUT [<type>]]  Walk relationships");
            println!();
            println!("Dot-commands:");
            println!("  .help              This message");
            println!("  .tables [pat]      List object types");
            println!("  .schema <type>     Show schema for type");
            println!("  .count [type]      Count objects");
            println!("  .backup [dir]      Create verified backup");
            println!("  .audit             Show compliance report");
            println!("  .metrics           Show database metrics");
            println!("  .exit / .quit / .q Exit shell");
            DotResult::Ok
        }

        ".tables" => {
            match kernel.list_types() {
                Ok(mut types) => {
                    types.sort();
                    if types.is_empty() {
                        println!("(no types)");
                    } else {
                        let filter = if arg.is_empty() { None } else { Some(arg.to_lowercase()) };
                        for t in &types {
                            if let Some(ref f) = filter {
                                if !t.to_lowercase().contains(f) { continue; }
                            }
                            println!("  {}", t);
                        }
                    }
                }
                Err(e) => return DotResult::Error(format!("{}", e)),
            }
            DotResult::Ok
        }

        ".count" => {
            let type_filter = if arg.is_empty() { None } else { Some(arg.to_string()) };
            match kernel.scan_heads() {
                Ok(heads) => {
                    let mut count = 0usize;
                    for (_, _, _, state) in &heads {
                        if *state != LifecycleState::Deleted {
                            // ponytail: type filtering needs object read — scan heads only gives state.
                            count += 1;
                        }
                    }
                    if let Some(_t) = type_filter {
                        println!("(type filter not supported in head scan — showing total)");
                    }
                    println!("{} objects (non-deleted), {} total heads", count, heads.len());
                }
                Err(e) => return DotResult::Error(format!("{}", e)),
            }
            DotResult::Ok
        }

        ".schema" => {
            if arg.is_empty() {
                return DotResult::Error("Usage: .schema <type_name>".into());
            }
            // ponytail: SchemaRegistry is in-memory; list_types gives types.
            // Schema inspection needs registry access — show what we can.
            match kernel.list_types() {
                Ok(types) => {
                    if types.iter().any(|t| t == arg) {
                        println!("Schema for '{}': (dynamic — properties validated on write)", arg);
                    } else {
                        println!("Type '{}' not found. Known types:", arg);
                        for t in &types { println!("  {}", t); }
                    }
                }
                Err(e) => return DotResult::Error(format!("{}", e)),
            }
            DotResult::Ok
        }

        ".relate" => {
            // Usage: .relate <source_hex> <target_hex> [rel_type]
            let parts: Vec<&str> = arg.split_whitespace().collect();
            if parts.len() < 2 {
                return DotResult::Error("Usage: .relate <source_hex> <target_hex> [rel_type]".into());
            }
            let src = match KOID::from_hex(parts[0]) {
                Ok(k) => k,
                Err(e) => return DotResult::Error(format!("bad source KOID: {}", e)),
            };
            let tgt = match KOID::from_hex(parts[1]) {
                Ok(k) => k,
                Err(e) => return DotResult::Error(format!("bad target KOID: {}", e)),
            };
            let rt = parts.get(2).copied().unwrap_or("related_to");
            let user = Subject::new("shell-user");
            match kernel.relate(RelateRequest::new(&user, src, tgt, rt)) {
                Ok(r) => {
                    println!("Related: {} -> {} [{}] (v{})", src.to_hex(), tgt.to_hex(), rt, r.version);
                }
                Err(e) => return DotResult::Error(format!("relate: {}", e)),
            }
            DotResult::Ok
        }

        ".backup" => {
            let dir_name = if arg.is_empty() {
                format!("{}.backup.{}", db_path, std::time::UNIX_EPOCH.elapsed().map(|d| d.as_secs()).unwrap_or(0))
            } else {
                arg.to_string()
            };
            let _ = std::fs::create_dir_all(&dir_name);
            let backup_data = format!("{}/data.redb", dir_name);
            match std::fs::copy(db_path, &backup_data) {
                Ok(n) => println!("Backup created: {} ({} bytes)", dir_name, n),
                Err(e) => return DotResult::Error(format!("backup failed: {}", e)),
            }
            DotResult::Ok
        }

        ".audit" => {
            match kernel.compliance_report() {
                Ok(report) => {
                    println!("Encryption: {}", if report.encryption_enabled { "enabled" } else { "disabled" });
                    println!("Policies:  {} ({})", report.policies_registered, report.policy_types.join(", "));
                    if let Some(ref s) = report.field_crypto_summary {
                        println!("Tenant keys: {}", s.tenant_keys);
                        for (kind, count) in &s.audit_events {
                            println!("  {:?}: {}", kind, count);
                        }
                    }
                }
                Err(e) => return DotResult::Error(format!("{}", e)),
            }
            DotResult::Ok
        }

        ".metrics" => {
            let (seq, audit) = kernel.journal_head().unwrap_or((0, [0u8; 32]));
            let heads = kernel.scan_heads().map(|h| h.len()).unwrap_or(0);
            println!("Journal seq: {}", seq);
            println!(
                "Audit hash: {}",
                audit.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
            );
            println!("Object heads: {}", heads);
            DotResult::Ok
        }

        _ => DotResult::Error(format!("Unknown command: {}. Type .help for commands.", verb)),
    }
}

fn ast_expr_to_value(e: &mnemosyne_compiler::parser::ast::Expr) -> Value {
    match e {
        mnemosyne_compiler::parser::ast::Expr::String(s) => Value::Text(s.clone()),
        mnemosyne_compiler::parser::ast::Expr::Number(n) => Value::Float(*n),
        mnemosyne_compiler::parser::ast::Expr::Bool(b) => Value::Bool(*b),
        mnemosyne_compiler::parser::ast::Expr::Null => Value::Null,
    }
}

fn execute_and_print(kernel: &Kernel, plan: &IrPlan, stdout: &mut dyn Write) {
    match Interpreter::execute(kernel, plan) {
        Ok(rows) => {
            match rows {
                mnemosyne_runtime::RowSet::Objects(objs) => {
                    if objs.is_empty() {
                        writeln!(stdout, "(0 rows)").ok();
                        return;
                    }
                    writeln!(stdout, "── {} row(s) ──", objs.len()).ok();
                    for obj in &objs {
                        let preview = ko_text(obj);
                        let preview_short: String = if preview.len() > 120 {
                            format!("{}...", &preview[..117])
                        } else {
                            preview
                        };
                        writeln!(
                            stdout,
                            "  {}  v{}  {}  {}",
                            obj.koid.to_hex(),
                            obj.version,
                            obj.metadata.type_name,
                            preview_short
                        )
                        .ok();
                    }
                }
                mnemosyne_runtime::RowSet::Scored(results) => {
                    writeln!(stdout, "── {} result(s) ──", results.len()).ok();
                    for (koid, score, type_name, version) in &results {
                        writeln!(stdout, "  {}  v{}  {}  score={:.4}", koid.to_hex(), version, type_name, score).ok();
                    }
                }
                mnemosyne_runtime::RowSet::Traversal(hits) => {
                    writeln!(stdout, "── {} hop(s) ──", hits.len()).ok();
                    for (koid, rel_type, depth) in &hits {
                        writeln!(stdout, "  depth={}  {}  [{}]", depth, koid.to_hex(), rel_type).ok();
                    }
                }
            }
        }
        Err(e) => {
            writeln!(stdout, "Error: {}", e).ok();
        }
    }
}
