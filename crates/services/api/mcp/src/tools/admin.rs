//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::{json, Kernel, LifecycleState, Ordering, Subject, ACTIVE_CONNECTIONS, J, SERVER_START};

// ---------------------------------------------------------------------------
// Design §22 storage admin tools — v2 is the only backend (launch S-02), so
// the admin capability is always present at runtime. The Option survives
// because the capability surfaces through the runtime `Opened` tuple; a None
// answers with an error inside the normal tool-result envelope, never a
// transport error.
// ---------------------------------------------------------------------------

pub(crate) fn tool_storage_stats(
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
) -> Result<J, String> {
    let admin = admin.ok_or("storage admin unavailable on this backend")?;
    let s = admin.storage_stats().map_err(|e| e.to_string())?;
    Ok(json!({
        "write": {
            "wal_bytes": s.write.wal_bytes,
            "flush_count": s.write.flush_count,
            "flush_latency_us": s.write.flush_latency_us,
            "fsync_count": s.write.fsync_count,
            "fsync_latency_us_buckets": s.write.fsync_latency_us_buckets,
            "compaction_backlog_bytes": s.write.compaction_backlog_bytes,
            "compaction_pending_segments": s.write.compaction_pending_segments,
            "checkpoint_count": s.write.checkpoint_count,
            "checkpoint_latency_us": s.write.checkpoint_latency_us,
            "write_queue_depth": s.write.write_queue_depth,
            "group_commit_batches": s.write.group_commit_batches,
            "group_commit_ops": s.write.group_commit_ops,
            "group_commit_max_ops": s.write.group_commit_max_ops,
            "last_compaction_ms": s.write.last_compaction_ms,
            "compaction_error_count": s.write.compaction_error_count,
            "recovery_ms": s.write.recovery_ms,
            "wal_replay_bytes": s.write.wal_replay_bytes,
        },
        "segments": {
            "count": s.segments.count,
            "bytes": s.segments.bytes,
        },
        "cache": {
            "hits": s.cache.hits,
            "misses": s.cache.misses,
            "evictions": s.cache.evictions,
            "bytes": s.cache.bytes,
        },
        "read": {
            "lookups": s.read.lookups,
            "get_wall_ns": s.read.get_wall_ns,
        },
    }))
}

pub(crate) fn tool_storage_compact(
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
) -> Result<J, String> {
    let admin = admin.ok_or("storage admin unavailable on this backend")?;
    let c = admin.storage_compact().map_err(|e| e.to_string())?;
    Ok(json!({
        "segments_in": c.segments_in,
        "segments_out": c.segments_out,
        "entries_in": c.entries_in,
        "entries_out": c.entries_out,
        "entries_archived": c.entries_archived,
    }))
}

pub(crate) fn tool_storage_checkpoint(
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
) -> Result<J, String> {
    let admin = admin.ok_or("storage admin unavailable on this backend")?;
    let c = admin.storage_checkpoint().map_err(|e| e.to_string())?;
    Ok(json!({"generation": c.generation}))
}

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

/// A v2-native backup dir holds exactly one `SNAPSHOT-{gen}` marker — its
/// presence is what routes restore/verify to the engine-native path.
fn snapshot_marker_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("SNAPSHOT-"))
        .map(|e| e.path())
}

pub(crate) fn tool_verify_backup(args: &J) -> Result<J, String> {
    let backup = args
        .get("backup")
        .and_then(|b| b.as_str())
        .ok_or("missing argument: backup")?;
    let meta_str = std::fs::read_to_string(format!("{}/meta.json", backup))
        .map_err(|e| format!("not a valid backup: {}", e))?;
    let meta: J = serde_json::from_str(&meta_str).map_err(|e| format!("bad meta: {}", e))?;
    let expected_seq = meta["journal_seq"].as_u64().unwrap_or(0);
    let expected_objects = meta["object_count"].as_u64().unwrap_or(0) as usize;
    // P3-M3: a v2-native backup verifies through its marker (decode +
    // checksum) — the only backup format post-S-02.
    let marker = snapshot_marker_in(std::path::Path::new(backup))
        .ok_or("not a v2-native backup: no SNAPSHOT marker")?;
    let ok = aikoql_storage_v2::snapshot::SnapshotMarker::read(&marker).is_ok();
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
    // PRR-3: surface semantic readiness (enrichment worker updates the static).
    let sem = crate::semantic_status_snapshot();
    Ok(json!({
        "status": if ready { "healthy" } else { "degraded" },
        "ready": ready,
        "journal_seq": seq,
        "journal_lag_ms": journal_lag_ms,
        "object_count": heads,
        "connection_pool": format!("{}/{}", connections, max_connections),
        "audit_hash": audit.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "uptime_seconds": SERVER_START.get().map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0),
        "semantic": {
            "state": sem.state,
            "detail": sem.detail,
        },
    }))
}

pub(crate) fn tool_backup(
    k: &Kernel,
    db_path: &str,
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
) -> Result<J, String> {
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

    // P3-M3 §58: the engine-native snapshot — pinned generation, verified
    // byte-for-byte, marker published LAST (its presence IS the commit
    // point, so `verified` needs no extra pass). Recovery-point metadata is
    // read BEFORE the snapshot pins the generation: the reported seq is a
    // point the snapshot contains.
    let admin = admin.ok_or("storage admin unavailable on this backend")?;
    let (seq, _audit) = k.journal_head().map_err(|e| e.to_string())?;
    let obj_count = k.scan_heads().map_err(|e| e.to_string())?.len();
    let info = admin.snapshot_to(&backup_dir).map_err(|e| e.to_string())?;
    let meta_path = backup_dir.join("meta.json");
    std::fs::write(
        &meta_path,
        json!({
            "timestamp": ts, "source": db_path, "journal_seq": seq,
            "object_count": obj_count, "engine": "aikoql-v2",
            "generation": info.generation, "file_count": info.file_count
        })
        .to_string(),
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({
        "backup": backup_dir, "timestamp": ts, "journal_seq": seq,
        "object_count": obj_count, "verified": true, "engine": "aikoql-v2",
        "generation": info.generation, "file_count": info.file_count,
        "bytes_copied": info.bytes_copied
    }))
}

pub(crate) fn tool_restore(
    args: &J,
    admin: Option<&dyn aikoql_storage_v2::engine::StorageAdminApi>,
) -> Result<J, String> {
    let backup = args
        .get("backup")
        .and_then(|b| b.as_str())
        .ok_or("missing argument: backup")?;
    let meta_str = std::fs::read_to_string(format!("{}/meta.json", backup))
        .map_err(|e| format!("not a valid backup: {}", e))?;
    let meta: J = serde_json::from_str(&meta_str).map_err(|e| format!("bad meta: {}", e))?;
    // P3-M3 §60: the engine-native path — verify, materialize, swap rows in
    // one frame. A v2-native backup (marker present) is the only backup
    // format post-S-02.
    let admin = admin.ok_or("storage admin unavailable on this backend")?;
    snapshot_marker_in(std::path::Path::new(backup))
        .ok_or("not a v2-native backup: no SNAPSHOT marker")?;
    let info = admin
        .restore_from(std::path::Path::new(backup))
        .map_err(|e| e.to_string())?;
    let pitr_seq = meta.get("journal_seq").and_then(|v| v.as_u64());
    let pitr_ts = meta.get("timestamp").and_then(|v| v.as_u64());
    Ok(json!({
        "restored": true,
        "engine": "aikoql-v2",
        "generation": info.generation,
        "rows_restored": info.rows_restored,
        "meta": meta,
        "recovery_point": {
            "journal_seq": pitr_seq,
            "timestamp": pitr_ts,
        }
    }))
}

pub(crate) fn tool_list_backups(db_path: &str) -> Result<J, String> {
    // Backups land next to the db file, not in the server's CWD.
    let dir = std::path::Path::new(db_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut backups = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(".backup.") {
                let meta_path = format!("{}/meta.json", dir.join(&name).display());
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

/// MRFC-0020 Phase 4 (IMPLEMENTATION-PLAN "Next implementation"): one
/// auditor export bundling the audit chain, the object inventory, the
/// PII-filtering config, the retention records, and the encryption
/// compliance report. Both frameworks carry the same bundle — the auditor
/// maps sections to clauses; the framework tag only labels the report.
/// Honest rows: purge coverage is counted-eligibility only (no kernel
/// purge op exists), and the PII detector's R8.1 known limits travel
/// with the pack rather than being implied away.
pub(crate) fn tool_evidence_pack(k: &Kernel, args: &J) -> Result<J, String> {
    let framework = args
        .get("framework")
        .and_then(|f| f.as_str())
        .unwrap_or("gdpr");
    if framework != "gdpr" && framework != "hipaa" {
        return Err(format!(
            "unsupported framework: {framework} (supported: gdpr, hipaa)"
        ));
    }

    // Audit chain + object inventory (audit_report substrate).
    let (seq, audit) = k.journal_head().map_err(|e| e.to_string())?;
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let mut by_state: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for (_, _, _, s) in &heads {
        *by_state.entry(s.to_string()).or_insert(0) += 1;
    }

    // Retention records (kernel-stamped valid_to horizons).
    let retention = k.retention_summary().map_err(|e| e.to_string())?;

    // Encryption compliance (existing report, same shape as its own tool).
    let encryption = tool_compliance_report(k)?;

    Ok(json!({
        "framework": framework,
        "audit_chain": audit.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "journal_seq": seq,
        "object_inventory": {
            "total": heads.len(),
            "by_state": by_state,
        },
        "pii_filtering": {
            "active": true,
            "detector_kinds": aikoql_ingestion::ALL_KINDS
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>(),
            // R8.1: pattern-based detection catches known formats only —
            // the known limits travel with the evidence, not implied away.
            "known_limits": "pattern-based detection catches known formats only; it does not decode URL-encoded or base64-encoded text or reassemble secrets split across lines (MRFC-0070 A7, R8.1)",
        },
        "retention": {
            "retained_objects": retention.retained_objects,
            "live_windows": retention.live_windows,
            "expired": retention.expired,
            "purge_coverage": "expired objects are counted and purge-eligible; physical deletion is caller-side — the kernel has no purge op (MRFC-0020 Phase 4 honest row)",
        },
        "encryption": encryption,
    }))
}

// ---------------------------------------------------------------------------
// index_create — P5-M17b (ND-14): the production declaration surface
// ---------------------------------------------------------------------------

/// Declare a property index (catalog row + registry + synchronous rebuild),
/// settle the maintainer, then analyze so the CBO can price the index.
/// Same call shape as the embedded SDK's create_index. Idempotent on
/// reopen: an existing declaration of the same shape rebuilds + re-analyzes
/// (the harness re-declares per cell to refresh M9 stats); a different
/// shape under the same name fails closed.
pub(crate) fn tool_index_create(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("index_create: 'name' (string) required")?;
    let type_name = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("index_create: 'type_name' (string) required")?;
    let properties: Vec<&str> = args
        .get("properties")
        .and_then(|v| v.as_array())
        .ok_or("index_create: 'properties' (array of strings) required")?
        .iter()
        .map(|p| p.as_str().ok_or("index_create: properties must be strings"))
        .collect::<Result<_, _>>()?;

    if let Some(d) = k
        .catalog_list_indexes()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|d| d.name == name)
    {
        if d.type_name != type_name || d.properties != properties {
            return Err(format!(
                "index_create: '{name}' already declared with a different shape"
            ));
        }
        k.rebuild_index(name).map_err(|e| e.to_string())?;
    } else {
        k.catalog_create_index(name, type_name, &properties)
            .map_err(|e| e.to_string())?;
    }

    // Settle the maintainer before analyze — a lagging re-apply can
    // transiently overwrite the rebuild with an older version (the M17b
    // wait_caught_up contract).
    if let Some(m) = k.index_maintainer() {
        m.wait_caught_up(k, std::time::Duration::from_secs(300))
            .map_err(|e| e.to_string())?;
    }

    let stats = k.analyze(type_name).map_err(|e| e.to_string())?;
    Ok(json!({
        "name": name,
        "type_name": type_name,
        "properties": properties,
        "rows": stats.row_count,
    }))
}
