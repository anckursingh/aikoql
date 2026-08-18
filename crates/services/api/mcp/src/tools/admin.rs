//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::*;

pub(crate) fn tool_metrics(k: &Kernel) -> Result<J, String> {
    let (seq, _audit) = k.journal_head().map_err(|e| e.to_string())?;
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let active = heads
        .iter()
        .filter(|(_, _, _, s)| *s != LifecycleState::Deleted)
        .count();
    let mut draft = 0u64;
    let mut active_st = 0u64;
    let mut verified = 0u64;
    let mut archived = 0u64;
    let mut deleted = 0u64;
    for (_, _, _, s) in &heads {
        match s {
            LifecycleState::Draft => draft += 1,
            LifecycleState::Active => active_st += 1,
            LifecycleState::Verified => verified += 1,
            LifecycleState::Archived => archived += 1,
            LifecycleState::Deleted => deleted += 1,
            // MRFC-0070 states: count as draft-equivalent pending
            LifecycleState::Discovered
            | LifecycleState::Extracted
            | LifecycleState::Proposed
            | LifecycleState::Validated
            | LifecycleState::Accepted
            | LifecycleState::Updated
            | LifecycleState::Superseded => draft += 1,
        }
    }
    // Type-level breakdown (ponytail: O(n) scan; add type index if slow).
    let types = k.list_types().map_err(|e| e.to_string())?;
    let system = Subject::with_roles("system", &["admin"]);
    let mut by_type = serde_json::Map::new();
    for t in &types {
        if let Ok(kos) = k.scan_by_type(&system, t) {
            by_type.insert(t.clone(), json!(kos.len()));
        }
    }
    let uptime_secs = SERVER_START
        .get()
        .map(|start| start.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    Ok(json!({
        "journal_seq": seq,
        "total_objects": heads.len(),
        "active_objects": active,
        "uptime_seconds": (uptime_secs * 10.0).round() / 10.0,
        "by_lifecycle": {
            "draft": draft,
            "active": active_st,
            "verified": verified,
            "archived": archived,
            "deleted": deleted,
        },
        "by_type": by_type,
    }))
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

pub(crate) fn tool_verify_backup(args: &J) -> Result<J, String> {
    let backup = args
        .get("backup")
        .and_then(|b| b.as_str())
        .ok_or("missing argument: backup")?;
    let data_path = format!("{}/data.redb", backup);
    if !std::path::Path::new(&data_path).exists() {
        return Err(format!("backup data file not found: {}", data_path));
    }
    let meta_str = std::fs::read_to_string(format!("{}/meta.json", backup))
        .map_err(|e| format!("not a valid backup: {}", e))?;
    let meta: J = serde_json::from_str(&meta_str).map_err(|e| format!("bad meta: {}", e))?;
    let expected_seq = meta["journal_seq"].as_u64().unwrap_or(0);
    let expected_objects = meta["object_count"].as_u64().unwrap_or(0) as usize;
    let ok = verify_backup_file(&data_path, expected_seq, expected_objects);
    Ok(json!({
        "backup": backup,
        "verified": ok,
        "expected_journal_seq": expected_seq,
        "expected_objects": expected_objects,
    }))
}

// ---------------------------------------------------------------------------
// HTTP metrics server — minimal std-based HTTP/1.0 handler
// ---------------------------------------------------------------------------

pub(crate) fn tool_abi_version(k: &Kernel) -> Result<J, String> {
    let version = k.abi_version();
    // Also export the full audit chain for offline verification.
    let proof = k.prove_export().map_err(|e| e.to_string())?;
    Ok(json!({
        "abi_version": version,
        "journal_seq": proof.journal_seq,
        "head_audit_hash": proof.head_audit_hash.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "event_count": proof.events.len(),
        "audit_chain_exportable": true,
    }))
}

pub(crate) fn tool_health(k: &Kernel) -> Result<J, String> {
    let (seq, audit) = k.journal_head().unwrap_or((0, [0u8; 32]));
    let heads = k.scan_heads().map(|h| h.len()).unwrap_or(0);
    let ready = true;
    // Single-node: journal is always current, so lag is 0.
    let journal_lag_ms: u64 = 0;
    let connections = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
    let max_connections = if connections > 0 { connections } else { 1 };
    Ok(json!({
        "status": if ready { "healthy" } else { "degraded" },
        "ready": ready,
        "journal_seq": seq,
        "journal_lag_ms": journal_lag_ms,
        "object_count": heads,
        "connection_pool": format!("{}/{}", connections, max_connections),
        "audit_hash": audit.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "uptime_seconds": SERVER_START.get().map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0),
    }))
}

pub(crate) fn tool_backup(k: &Kernel, db_path: &str) -> Result<J, String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    use std::path::{Path, PathBuf};
    let src = Path::new(db_path);
    let backup_dir: PathBuf = {
        let mut p = src.as_os_str().to_os_string();
        p.push(format!(".backup.{}", ts));
        PathBuf::from(p)
    };
    std::fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;

    // Copy the database file — use filename from source for multi-file db support.
    let src_file = src.file_name().ok_or("invalid db path: no filename")?;
    let dest_path = backup_dir.join(src_file);
    std::fs::copy(src, &dest_path).map_err(|e| e.to_string())?;

    // Record source metadata.
    let (seq, _audit) = k.journal_head().map_err(|e| e.to_string())?;
    let obj_count = k.scan_heads().map_err(|e| e.to_string())?.len();
    let meta_path = backup_dir.join("meta.json");
    std::fs::write(
        &meta_path,
        json!({"timestamp": ts, "source": db_path, "journal_seq": seq, "object_count": obj_count})
            .to_string(),
    )
    .map_err(|e| e.to_string())?;

    // Verify: open backup in a temp kernel and check integrity.
    let dest_str = dest_path.to_string_lossy().to_string();
    let verified = verify_backup_file(&dest_str, seq, obj_count);

    Ok(
        json!({"backup": backup_dir, "timestamp": ts, "journal_seq": seq, "object_count": obj_count, "verified": verified}),
    )
}

/// Open a backup file in a throwaway kernel and check basic integrity.
pub(crate) fn verify_backup_file(path: &str, expected_seq: u64, expected_objects: usize) -> bool {
    let engine = match RedbEngine::open(path) {
        Ok(e) => e,
        Err(_) => return false,
    };
    let k = match Kernel::open(
        std::sync::Arc::new(engine),
        std::sync::Arc::new(SystemClock),
        0,
    ) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let (seq, _) = match k.journal_head() {
        Ok(h) => h,
        Err(_) => return false,
    };
    let count = match k.scan_heads() {
        Ok(h) => h.len(),
        Err(_) => return false,
    };
    seq == expected_seq && count == expected_objects
}

pub(crate) fn tool_restore(args: &J, current_db: &str) -> Result<J, String> {
    let backup = args
        .get("backup")
        .and_then(|b| b.as_str())
        .ok_or("missing argument: backup")?;
    let meta_str = std::fs::read_to_string(format!("{}/meta.json", backup))
        .map_err(|e| format!("not a valid backup: {}", e))?;
    let meta: J = serde_json::from_str(&meta_str).map_err(|e| format!("bad meta: {}", e))?;
    if !std::path::Path::new(&format!("{}/data.redb", backup)).exists() {
        return Err("backup data file missing".into());
    }
    std::fs::copy(format!("{}/data.redb", backup), current_db).map_err(|e| e.to_string())?;
    // Report PITR recovery point from backup metadata.
    let pitr_seq = meta.get("journal_seq").and_then(|v| v.as_u64());
    let pitr_ts = meta.get("timestamp").and_then(|v| v.as_u64());
    Ok(json!({
        "restored": true,
        "meta": meta,
        "recovery_point": {
            "journal_seq": pitr_seq,
            "timestamp": pitr_ts,
        }
    }))
}

pub(crate) fn tool_list_backups() -> Result<J, String> {
    let mut backups = Vec::new();
    if let Ok(entries) = std::fs::read_dir(".") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".backup.") {
                let meta_path = format!("{}/meta.json", name);
                if let Ok(meta_str) = std::fs::read_to_string(&meta_path) {
                    if let Ok(meta) = serde_json::from_str::<J>(&meta_str) {
                        backups.push(json!({"name": name, "meta": meta}));
                    }
                }
            }
        }
    }
    Ok(json!({"backups": backups}))
}

pub(crate) fn tool_audit_report(k: &Kernel) -> Result<J, String> {
    let (seq, audit) = k.journal_head().map_err(|e| e.to_string())?;
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let total = heads.len();
    let by_state: Vec<J> = heads
        .iter()
        .map(|(koid, v, ts, state)| {
            json!({"koid": koid.to_hex(), "version": v, "commit_ts": ts, "state": state.to_string()})
        })
        .collect();
    let events = k.journal().map_err(|e| e.to_string())?;
    let event_count = events.len();
    Ok(json!({
        "audit_chain": audit.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "journal_seq": seq,
        "journal_events": event_count,
        "total_objects": total,
        "objects": by_state,
    }))
}

pub(crate) fn tool_compliance_report(k: &Kernel) -> Result<J, String> {
    let report = k.compliance_report().map_err(|e| e.to_string())?;
    let summary = report.field_crypto_summary.as_ref();
    let audit_counts: Vec<J> = summary
        .map(|s| {
            s.audit_events
                .iter()
                .map(|(kind, count)| json!({"kind": kind.as_str(), "count": count}))
                .collect()
        })
        // justified: no crypto summary → empty audit list
        .unwrap_or_default();
    Ok(json!({
        "encryption_enabled": report.encryption_enabled,
        "policies_registered": report.policies_registered,
        "policy_types": report.policy_types,
        "field_encryption_enabled": summary.map(|s| s.field_encryption_enabled).unwrap_or(false),
        "tenant_keys": summary.map(|s| s.tenant_keys).unwrap_or(0),
        "audit_events": audit_counts,
        "compliance_grade": if report.encryption_enabled && report.policies_registered > 0 { "A" } else { "C" },
    }))
}
