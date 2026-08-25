//! CLI subcommands: backup/restore/audit/report/ingest-dir/import/keygen.
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::admin::*;
use crate::imports::*;
use crate::ingest::*;
use crate::model::*;
pub(crate) fn print_usage() {
    println!(concat!(
        "aikoql — Knowledge Database Suite\n",
        "\n",
        "Usage: aikoql-mcp <COMMAND> [OPTIONS]\n",
        "\n",
        "Commands:\n",
        "  shell [DB]             Interactive aikoql shell (REPL)\n",
        "  serve [OPTIONS] [DB]   Start MCP server (default: stdio mode)\n",
        "  backup [DB]            Create a verified backup\n",
        "  restore BACKUP [DB]    Restore from a backup\n",
        "  audit [DB]             Print encryption compliance report\n",
        "  keygen [PATH]          Generate an encryption master key\n",
        "  import <SOURCE> [ARGS]  Import from DB (postgres, sqlite, mongodb)\n",
        "  ingest-dir [PATH] [DB] [--parallel] [--incremental] [--model-dir DIR] Ingest directory into knowledge base\n",
        "  report [PATH]          Print knowledge report for directory\n",
        "  model install [MODEL]  Install an embedding model into the local store (offline use)\n",
        "\n",
        "Server options (serve mode):\n",
        "  --listen ADDR          TCP listen address (e.g., 127.0.0.1:9090; empty host = loopback)\n",
        "  --tcp-token SPEC       TCP auth: TOKEN[:TENANT[:ROLE1,ROLE2]] (repeatable, required with --listen)\n",
        "                         (dev-only: tokens hit the process list — set AIKOQL_TCP_TOKEN or\n",
        "                         AIKOQL_TCP_TOKEN_FILE in production instead; env replaces flags)\n",
        "  --metrics-addr ADDR    HTTP metrics + health endpoint (e.g., 127.0.0.1:9091)\n",
        "  --embedding-provider P  Embedding provider: \"candle\" (default), \"http\", or \"ollama\"\n",
        "  --embedding-base-url U  OpenAI-compatible base URL (default: http://localhost:11434)\n",
        "  --embedding-model M     Model name (default: nomic-embed-text)\n",
        "  --embedding-api-key K   API key for remote endpoints (omit for Ollama)\n",
        "  --model-dir DIR         Local embedding model store (default: ~/.aikoql/models)\n",
        "  --config PATH           TOML config file (auto: ./aikoql.toml, then /etc/aikoql/aikoql.toml)\n",
        "\n",
        "Precedence: defaults < aikoql.toml < env (AIKOQL_*) < CLI flags.\n",
        "\n",
        "Examples:\n",
        "  aikoql-mcp shell                           # Interactive shell\n",
        "  aikoql-mcp shell :memory:                  # In-memory shell\n",
        "  aikoql-mcp serve                           # Stdio MCP server\n",
        "  aikoql-mcp serve --listen :9090 --tcp-token s3cret:acme:admin ./kb.redb\n",
        "  aikoql-mcp serve --listen :9090 --tcp-token s3cret:acme:admin --metrics-addr :9091 ./kb.redb\n",
        "  aikoql-mcp backup ./kb.redb                # Create backup\n",
        "  aikoql-mcp restore kb.redb.backup.12345    # Restore backup\n",
        "  aikoql-mcp audit                           # Compliance report\n",
        "  aikoql-mcp keygen ./master.key             # Generate key\n",
        "  aikoql-mcp import 'host=localhost db=mydb'   # Import from PostgreSQL\n",
        "  aikoql-mcp ingest-dir                        # Ingest CWD (sequential)\n",
        "  aikoql-mcp ingest-dir ~/my-project            # Ingest specific path\n",
        "  aikoql-mcp ingest-dir . --parallel            # Parallel ingestion\n",
        "  aikoql-mcp ingest-dir . --incremental         # Incremental (git-tracked)\n",
        "  aikoql-mcp report                            # Report on CWD\n",
        "  aikoql-mcp report ~/my-project                # Report on specific path\n",
    ));
}

// ---------------------------------------------------------------------------

/// CLI subcommand dispatch (R7: moved verbatim from main()). Returns true
/// when a subcommand ran; false falls through to server mode.
pub(crate) fn dispatch(args: &[String], subcmd: Option<&str>, subcmd_idx: Option<usize>) -> bool {
    let arg_after = subcmd_idx.and_then(|i| args.get(i + 2)).map(String::as_str);
    let arg_after2 = subcmd_idx.and_then(|i| args.get(i + 3)).map(String::as_str);

    match subcmd {
        Some("shell") => {
            // Accept: aikoql-mcp shell [--tenant NAME] [DB_PATH]
            let Some(idx) = subcmd_idx else {
                eprintln!("Usage: aikoql-mcp shell [--tenant NAME] [DB_PATH]");
                std::process::exit(2);
            };
            let mut db = "./aikoql.redb";
            let mut tenant: Option<&str> = None;
            let tail_args: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
            let mut ti = 0;
            while ti < tail_args.len() {
                match tail_args[ti] {
                    "--tenant" => {
                        if ti + 1 < tail_args.len() {
                            tenant = Some(tail_args[ti + 1]);
                            ti += 2;
                        } else {
                            ti += 1;
                        }
                    }
                    _ => {
                        db = tail_args[ti];
                        ti += 1;
                    }
                }
            }
            crate::shell::run_shell(db, tenant);
            true
        }
        Some("backup") => {
            run_backup(arg_after.unwrap_or("./aikoql.redb"));
            true
        }
        Some("restore") => {
            let backup = arg_after.unwrap_or_else(|| {
                eprintln!("Usage: aikoql-mcp restore <BACKUP_DIR> [DB_PATH]");
                std::process::exit(1);
            });
            let target = arg_after2.unwrap_or("./aikoql.redb");
            run_restore(backup, target);
            true
        }
        Some("audit") => {
            run_audit(arg_after.unwrap_or("./aikoql.redb"));
            true
        }
        Some("ingest-dir") => {
            let Some(idx) = subcmd_idx else {
                eprintln!("Usage: aikoql-mcp ingest-dir [PATH] [DB] [--parallel] [--incremental] [--model-dir DIR]");
                std::process::exit(2);
            };
            let path = arg_after.unwrap_or(".");
            let db = arg_after2.unwrap_or("./aikoql.redb");
            let mut parallel = false;
            let mut incremental = false;
            let mut model_dir: Option<String> = None;
            let tail_args: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
            let mut ti = 0;
            while ti < tail_args.len() {
                match tail_args[ti] {
                    "--parallel" => {
                        parallel = true;
                        ti += 1;
                    }
                    "--incremental" => {
                        incremental = true;
                        ti += 1;
                    }
                    "--model-dir" => {
                        model_dir = tail_args.get(ti + 1).map(|s| s.to_string());
                        if model_dir.is_none() {
                            eprintln!("--model-dir requires a value");
                            std::process::exit(2);
                        }
                        ti += 2;
                    }
                    _ => ti += 1,
                }
            }
            run_ingest_dir(path, db, parallel, incremental, model_dir.as_deref());
            true
        }
        Some("model") => {
            let Some(idx) = subcmd_idx else {
                eprintln!("Usage: aikoql-mcp model install [MODEL_ID] [--model-dir DIR]");
                std::process::exit(2);
            };
            let tail_args: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
            match tail_args.first().copied() {
                Some("install") => {
                    let model_id = tail_args
                        .get(1)
                        .copied()
                        .filter(|s| !s.starts_with("--"))
                        .unwrap_or(aikoql_semantic::provider::DEFAULT_MODEL_ID);
                    let mut model_dir: Option<String> = None;
                    let mut ti = 2;
                    while ti < tail_args.len() {
                        if tail_args[ti] == "--model-dir" {
                            model_dir = tail_args.get(ti + 1).map(|s| s.to_string());
                            if model_dir.is_none() {
                                eprintln!("--model-dir requires a value");
                                std::process::exit(2);
                            }
                            ti += 2;
                        } else {
                            ti += 1;
                        }
                    }
                    run_model_install(model_id, model_dir.as_deref());
                }
                _ => {
                    eprintln!("Usage: aikoql-mcp model install [MODEL_ID] [--model-dir DIR]");
                    eprintln!(
                        "Installs an embedding model into the local store for offline use. The"
                    );
                    eprintln!(
                        "server and ingest-dir never download — they load installed models only."
                    );
                    std::process::exit(2);
                }
            }
            true
        }
        Some("report") => {
            let path = arg_after.unwrap_or(".");
            run_report(path);
            true
        }
        Some("import") => {
            // import <source> <source-args...>
            //   import postgres <conn_str> [--tenant NAME] [--table TABLE] [DB_PATH]
            //   import sqlite <file.db> [--tenant NAME] [--table TABLE] [DB_PATH]
            let Some(idx) = subcmd_idx else {
                eprintln!("Usage: aikoql-mcp import <SOURCE> [ARGS...]");
                std::process::exit(2);
            };
            let ti_args: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
            if ti_args.is_empty() {
                eprintln!("Usage: aikoql-mcp import <SOURCE> [ARGS...]");
                eprintln!("Sources: postgres, sqlite, mongodb, neo4j");
                eprintln!("  import postgres <CONN_STR> [--tenant NAME] [--table TABLE] [--run-id ID] [DB_PATH]");
                eprintln!("  import sqlite <FILE.db> [--tenant NAME] [--table TABLE] [DB_PATH]");
                eprintln!(
                    "  import mongodb <URI> --db <NAME> [--collection C] [--tenant T] [DB_PATH]"
                );
                eprintln!("  import neo4j <URI> [--user U] [--password P] [--label L] [--tenant T] [--run-id ID] [DB_PATH]");
                std::process::exit(1);
            }
            match ti_args[0] {
                "postgres" => {
                    let mut conn_str: Option<&str> = None;
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut table_filter: Option<&str> = None;
                    let mut run_id = fresh_run_id();
                    let mut ti = 1;
                    while ti < ti_args.len() {
                        match ti_args[ti] {
                            "--tenant" => {
                                if ti + 1 < ti_args.len() {
                                    tenant = Some(ti_args[ti + 1]);
                                    ti += 2;
                                } else {
                                    ti += 1;
                                }
                            }
                            "--table" => {
                                if ti + 1 < ti_args.len() {
                                    table_filter = Some(ti_args[ti + 1]);
                                    ti += 2;
                                } else {
                                    ti += 1;
                                }
                            }
                            "--run-id" => {
                                if ti + 1 < ti_args.len() {
                                    run_id = ti_args[ti + 1].to_string();
                                    ti += 2;
                                } else {
                                    ti += 1;
                                }
                            }
                            _ if !ti_args[ti].starts_with("--") => {
                                if conn_str.is_none() {
                                    conn_str = Some(ti_args[ti]);
                                    ti += 1;
                                } else {
                                    target_db = ti_args[ti];
                                    ti += 1;
                                }
                            }
                            _ => {
                                ti += 1;
                            }
                        }
                    }
                    let cs = conn_str.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import postgres <CONN_STR> [--tenant NAME] [--table TABLE] [--run-id ID] [DB_PATH]");
                        std::process::exit(1);
                    });
                    run_pg_import(cs, target_db, tenant, table_filter, &run_id);
                }
                "neo4j" => {
                    let mut uri: Option<&str> = None;
                    let mut user = "neo4j";
                    let mut password = "password";
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut label_filter: Option<&str> = None;
                    let mut run_id = fresh_run_id();
                    let mut ni = 1;
                    while ni < ti_args.len() {
                        match ti_args[ni] {
                            "--user" => {
                                if ni + 1 < ti_args.len() {
                                    user = ti_args[ni + 1];
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            "--password" => {
                                if ni + 1 < ti_args.len() {
                                    password = ti_args[ni + 1];
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            "--tenant" => {
                                if ni + 1 < ti_args.len() {
                                    tenant = Some(ti_args[ni + 1]);
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            "--label" => {
                                if ni + 1 < ti_args.len() {
                                    label_filter = Some(ti_args[ni + 1]);
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            "--run-id" => {
                                if ni + 1 < ti_args.len() {
                                    run_id = ti_args[ni + 1].to_string();
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            _ if !ti_args[ni].starts_with("--") => {
                                if uri.is_none() {
                                    uri = Some(ti_args[ni]);
                                    ni += 1;
                                } else {
                                    target_db = ti_args[ni];
                                    ni += 1;
                                }
                            }
                            _ => {
                                ni += 1;
                            }
                        }
                    }
                    let u = uri.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import neo4j <URI> [--user U] [--password P] [--label L] [--tenant T] [--run-id ID] [DB_PATH]");
                        std::process::exit(1);
                    });
                    run_neo4j_import(u, user, password, target_db, tenant, label_filter, &run_id);
                }
                "mongodb" => {
                    let mut uri: Option<&str> = None;
                    let mut database: Option<&str> = None;
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut coll_filter: Option<&str> = None;
                    let mut run_id = fresh_run_id();
                    let mut mi = 1;
                    while mi < ti_args.len() {
                        match ti_args[mi] {
                            "--db" | "--database" => {
                                if mi + 1 < ti_args.len() {
                                    database = Some(ti_args[mi + 1]);
                                    mi += 2;
                                } else {
                                    mi += 1;
                                }
                            }
                            "--tenant" => {
                                if mi + 1 < ti_args.len() {
                                    tenant = Some(ti_args[mi + 1]);
                                    mi += 2;
                                } else {
                                    mi += 1;
                                }
                            }
                            "--collection" => {
                                if mi + 1 < ti_args.len() {
                                    coll_filter = Some(ti_args[mi + 1]);
                                    mi += 2;
                                } else {
                                    mi += 1;
                                }
                            }
                            "--run-id" => {
                                if mi + 1 < ti_args.len() {
                                    run_id = ti_args[mi + 1].to_string();
                                    mi += 2;
                                } else {
                                    mi += 1;
                                }
                            }
                            _ if !ti_args[mi].starts_with("--") => {
                                if uri.is_none() {
                                    uri = Some(ti_args[mi]);
                                    mi += 1;
                                } else if database.is_none() {
                                    database = Some(ti_args[mi]);
                                    mi += 1;
                                } else {
                                    target_db = ti_args[mi];
                                    mi += 1;
                                }
                            }
                            _ => {
                                mi += 1;
                            }
                        }
                    }
                    let u = uri.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import mongodb <URI> --db <NAME> [--collection C] [--tenant T] [--run-id ID] [DB_PATH]");
                        std::process::exit(1);
                    });
                    let db = database.unwrap_or_else(|| {
                        eprintln!("Missing --db <DATABASE_NAME>");
                        std::process::exit(1);
                    });
                    run_mongo_import(u, db, target_db, tenant, coll_filter, &run_id);
                }
                "sqlite" => {
                    let mut source_file: Option<&str> = None;
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut table_filter: Option<&str> = None;
                    let mut si = 1;
                    while si < ti_args.len() {
                        match ti_args[si] {
                            "--tenant" => {
                                if si + 1 < ti_args.len() {
                                    tenant = Some(ti_args[si + 1]);
                                    si += 2;
                                } else {
                                    si += 1;
                                }
                            }
                            "--table" => {
                                if si + 1 < ti_args.len() {
                                    table_filter = Some(ti_args[si + 1]);
                                    si += 2;
                                } else {
                                    si += 1;
                                }
                            }
                            _ if !ti_args[si].starts_with("--") => {
                                if source_file.is_none() {
                                    source_file = Some(ti_args[si]);
                                    si += 1;
                                } else {
                                    target_db = ti_args[si];
                                    si += 1;
                                }
                            }
                            _ => {
                                si += 1;
                            }
                        }
                    }
                    let sf = source_file.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import sqlite <FILE.db> [--tenant NAME] [--table TABLE] [DB_PATH]");
                        std::process::exit(1);
                    });
                    run_sqlite_import(sf, target_db, tenant, table_filter);
                }
                other => {
                    eprintln!(
                        "Unknown import source: {}. Supported: postgres, sqlite",
                        other
                    );
                    std::process::exit(1);
                }
            }
            true
        }
        Some("keygen") => {
            run_keygen(arg_after.unwrap_or("./aikoql.key"));
            true
        }
        Some("help") => {
            print_usage();
            true
        }
        _ => false,
    }
}
