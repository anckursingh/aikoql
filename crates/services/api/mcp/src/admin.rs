//! Subcommand runners extracted verbatim from cli.rs (PRR-7).
//! No behavior changes.

use crate::*;

pub(crate) fn run_backup(db_path: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir_name = format!("{}.backup.{}", db_path, ts);
    if let Err(e) = std::fs::create_dir_all(&dir_name) {
        eprintln!("create backup dir: {}", e);
        std::process::exit(1);
    }

    // Gather metadata, then drop kernel to release file lock before copy.
    let (seq, object_count) = {
        let engine = match RedbEngine::open(db_path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("open source db: {}", e);
                std::process::exit(1);
            }
        };
        let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("open kernel: {}", e);
                std::process::exit(1);
            }
        };
        let s = kernel
            .journal_head()
            .unwrap_or_else(|e| {
                eprintln!("Error reading journal: {}", e);
                std::process::exit(1);
            })
            .0;
        let n = kernel
            .scan_heads()
            .unwrap_or_else(|e| {
                eprintln!("Error scanning heads: {}", e);
                std::process::exit(1);
            })
            .len();
        (s, n)
    }; // kernel + engine dropped → file lock released.

    let data_path = format!("{}/data.redb", dir_name);
    if let Err(e) = std::fs::copy(db_path, &data_path) {
        eprintln!("copy db file: {}", e);
        std::process::exit(1);
    }

    let meta = serde_json::json!({
        "journal_seq": seq,
        "object_count": object_count,
        "backup_ts": ts,
        "source": db_path,
    });
    let meta_json = serde_json::to_string_pretty(&meta).unwrap_or_else(|e| {
        eprintln!("write backup meta: {}", e);
        std::process::exit(1);
    });
    if let Err(e) = std::fs::write(format!("{}/meta.json", dir_name), meta_json) {
        eprintln!("write backup meta: {}", e);
        std::process::exit(1);
    }

    println!("Backup created: {}", dir_name);
    println!("  Objects: {}", object_count);
    println!("  Journal seq: {}", seq);
}
pub(crate) fn run_restore(backup_dir: &str, target_path: &str) {
    let data_path = format!("{}/data.redb", backup_dir);
    if !std::path::Path::new(&data_path).exists() {
        eprintln!("Error: not a valid backup — {} not found", data_path);
        std::process::exit(1);
    }
    let meta_path = format!("{}/meta.json", backup_dir);
    if let Ok(meta_str) = std::fs::read_to_string(&meta_path) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_str) {
            println!("Restoring from: {}", backup_dir);
            println!(
                "  Original source: {}",
                meta.get("source").and_then(|s| s.as_str()).unwrap_or("?")
            );
            println!(
                "  Object count: {}",
                meta.get("object_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
            println!(
                "  Journal seq: {}",
                meta.get("journal_seq")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
        }
    }
    if let Err(e) = std::fs::copy(&data_path, target_path) {
        eprintln!("restore copy: {}", e);
        std::process::exit(1);
    }
    println!("Restored to: {}", target_path);
}
pub(crate) fn run_audit(db_path: &str) {
    let engine = match RedbEngine::open(db_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open db: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    match kernel.compliance_report() {
        Ok(report) => {
            let json = serde_json::to_string_pretty(&serde_json::json!({
                "encryption_enabled": report.encryption_enabled,
                "policies_registered": report.policies_registered,
                "policy_types": report.policy_types,
            }))
            .unwrap_or_else(|e| e.to_string());
            println!("{}", json);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
pub(crate) fn run_report(path: &str) {
    eprintln!("Analyzing directory: {}\n", path);

    let result = match aikoql_ingestion::ingest_directory(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let report = aikoql_ingestion::build_report(
        &result.ir,
        path,
        result.files_processed,
        result.files_skipped,
        result.dirs_skipped,
        result.binary_skipped,
    );
    println!("{}", aikoql_ingestion::format_report(&report));
}
pub(crate) fn run_keygen(path: &str) {
    use aikoql_kernel::security::crypto::{Aes256Gcm, CryptoProvider};
    let key = Aes256Gcm::new().generate_key();
    let hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
    if path == "-" {
        println!("{}", hex);
    } else {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!("create key dir: {}", e);
                    std::process::exit(1);
                }
            }
        }
        if let Err(e) = std::fs::write(path, &hex) {
            eprintln!("write key file: {}", e);
            std::process::exit(1);
        }
        println!("Key written to: {}", path);
        println!("Set encryption.key_path in aikoql.toml to this path.");
        println!(
            "Restrict file permissions: chmod 600 {} (Linux) or equivalent.",
            path
        );
    }
}
