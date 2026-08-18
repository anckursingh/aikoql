//! Key Audit Log — MRFC-0020 Phase 4.
//!
//! Immutable append-only log of key lifecycle events. Each event is a
//! versioned, timestamped record stored under `__audit__/keys/` in the
//! storage engine. The audit log is independent of the KnowledgeEvent
//! journal — it's a separate concern with its own prefix.
//!
//! Integration points:
//! - Envelope: log on tenant key creation and KEK rotation
//! - FieldCrypto: log on first encrypt/decrypt per tenant (usage audit)
//! - KMS: log on master key creation and rotation
//!
//! Format: version(1) || ts_ms_be(8) || kind(1) || detail_len_be(2) || detail

use crate::storage::store::{StorageEngine, WriteBatch};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Key event types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyEventKind {
    /// A new key was created (master, tenant DEK, or field key).
    Created = 0x01,
    /// A key was rotated (new key material, old decommissioned).
    Rotated = 0x02,
    /// A key was used for encryption or decryption (logged on first use).
    Used = 0x03,
    /// A key operation failed (wrong key, corrupted data, etc.).
    Failure = 0x04,
}

impl KeyEventKind {
    pub fn as_str(&self) -> &str {
        match self {
            KeyEventKind::Created => "created",
            KeyEventKind::Rotated => "rotated",
            KeyEventKind::Used => "used",
            KeyEventKind::Failure => "failure",
        }
    }
}

/// A single key audit event, serialized to the audit log.
#[derive(Clone, Debug)]
pub struct KeyEvent {
    pub kind: KeyEventKind,
    /// Which key layer: "master", "kek", "tenant-dek:acme", "field:surname", etc.
    pub key_label: String,
    /// Human-readable detail (e.g., "tenant DEK created for acme", "rotation failed: I/O error").
    pub detail: String,
    /// Milliseconds since epoch.
    pub ts_ms: u64,
}

impl KeyEvent {
    pub fn now(kind: KeyEventKind, key_label: &str, detail: &str) -> Self {
        KeyEvent {
            kind,
            key_label: key_label.to_string(),
            detail: detail.to_string(),
            ts_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    /// Encode to key-value form: (key, value).
    /// Key: `__audit__/keys/<ts_ms_be_8>/<seq_4>`
    /// Value: kind(1) || key_label_len_be(2) || key_label || detail_len_be(2) || detail
    fn encode(&self, seq: u32) -> (Vec<u8>, Vec<u8>) {
        let key = {
            let mut k = Vec::with_capacity(20 + 8 + 4);
            k.extend_from_slice(b"__audit__/keys/");
            k.extend_from_slice(&self.ts_ms.to_be_bytes());
            k.push(b'/');
            k.extend_from_slice(&seq.to_be_bytes());
            k
        };
        let val = {
            let lb = self.key_label.as_bytes();
            let db = self.detail.as_bytes();
            let mut v = Vec::with_capacity(1 + 2 + lb.len() + 2 + db.len());
            v.push(self.kind as u8);
            v.extend_from_slice(&(lb.len() as u16).to_be_bytes());
            v.extend_from_slice(lb);
            v.extend_from_slice(&(db.len() as u16).to_be_bytes());
            v.extend_from_slice(db);
            v
        };
        (key, val)
    }

    /// Decode from value bytes. Returns None on corrupt data.
    fn decode(key_label_expected: Option<&str>, val: &[u8]) -> Option<Self> {
        if val.is_empty() {
            return None;
        }
        let kind = match val[0] {
            0x01 => KeyEventKind::Created,
            0x02 => KeyEventKind::Rotated,
            0x03 => KeyEventKind::Used,
            0x04 => KeyEventKind::Failure,
            _ => return None,
        };
        if val.len() < 3 {
            return None;
        }
        let kl_len = u16::from_be_bytes([val[1], val[2]]) as usize;
        let kl_start = 3usize;
        let kl_end = kl_start + kl_len;
        if val.len() < kl_end + 2 {
            return None;
        }
        let key_label = String::from_utf8_lossy(&val[kl_start..kl_end]).into_owned();
        if let Some(expected) = key_label_expected {
            if key_label != expected {
                return None;
            }
        }
        let d_len = u16::from_be_bytes([val[kl_end], val[kl_end + 1]]) as usize;
        let d_start = kl_end + 2;
        let d_end = d_start + d_len;
        if val.len() < d_end {
            return None;
        }
        let detail = String::from_utf8_lossy(&val[d_start..d_end]).into_owned();
        Some(KeyEvent {
            kind,
            key_label,
            detail,
            ts_ms: 0, // caller fills from the key prefix
        })
    }
}

// ---------------------------------------------------------------------------
// KeyAuditLog
// ---------------------------------------------------------------------------

/// Append-only audit log stored under `__audit__/keys/`.
/// Thread-safe: all writes go through a single StorageEngine reference.
pub struct KeyAuditLog {
    store: Arc<dyn StorageEngine>,
    /// Monotonic sequence number per millisecond (reset on ts change).
    /// ponytail: global Mutex is fine — audit writes are rate-limited by
    /// human-scale key operations (creation, rotation); never on hot path.
    seq: std::sync::Mutex<(u64, u32)>,
}

impl KeyAuditLog {
    pub fn new(store: Arc<dyn StorageEngine>) -> Self {
        KeyAuditLog {
            store,
            seq: std::sync::Mutex::new((0, 0)),
        }
    }

    /// Append a key event to the audit log. Returns the storage key.
    pub fn record(&self, event: &KeyEvent) -> Result<Vec<u8>, String> {
        // justified: Mutex poison is unrecoverable
        let mut guard = self.seq.lock().unwrap();
        if guard.0 != event.ts_ms {
            guard.0 = event.ts_ms;
            guard.1 = 0;
        }
        let seq = guard.1;
        guard.1 += 1;

        let (key, val) = event.encode(seq);
        let mut batch = WriteBatch::new();
        batch.put(key.clone(), val);
        self.store
            .write_batch(&batch)
            .map_err(|e| format!("audit write: {}", e))?;
        Ok(key)
    }

    /// Scan audit events, optionally filtered by key label prefix.
    /// Returns events ordered by timestamp (oldest first).
    pub fn scan(&self, label_prefix: Option<&str>, limit: usize) -> Result<Vec<KeyEvent>, String> {
        let prefix = b"__audit__/keys/";
        // ponytail: naive scan — O(n) over audit log. Fine until millions of
        // events; add time-range partitioning when audit exceeds 100k records.
        let all = self
            .store
            .scan(prefix)
            .map_err(|e| format!("audit scan: {}", e))?;
        let mut events: Vec<KeyEvent> = all
            .iter()
            .filter_map(|(k, v)| {
                let ts_ms = decode_ts_from_audit_key(k)?;
                let mut ev = KeyEvent::decode(None, v)?;
                ev.ts_ms = ts_ms;
                if let Some(lp) = label_prefix {
                    if !ev.key_label.starts_with(lp) {
                        return None;
                    }
                }
                Some(ev)
            })
            .collect();
        events.sort_by_key(|e| e.ts_ms);
        if events.len() > limit {
            events.truncate(limit);
        }
        Ok(events)
    }

    /// Count events by kind (for compliance reports).
    pub fn counts_by_kind(&self) -> Result<Vec<(KeyEventKind, usize)>, String> {
        let all = self
            .store
            .scan(b"__audit__/keys/")
            .map_err(|e| format!("audit count: {}", e))?;
        let mut counts = [0usize; 5]; // index 1..=4
        for (_, v) in &all {
            if v.is_empty() {
                continue;
            }
            let idx = v[0] as usize;
            if (1..=4).contains(&idx) {
                counts[idx] += 1;
            }
        }
        Ok(vec![
            (KeyEventKind::Created, counts[1]),
            (KeyEventKind::Rotated, counts[2]),
            (KeyEventKind::Used, counts[3]),
            (KeyEventKind::Failure, counts[4]),
        ])
    }
}

/// Extract the timestamp from an audit key: `__audit__/keys/<ts_ms_be_8>/...`
fn decode_ts_from_audit_key(key: &[u8]) -> Option<u64> {
    if key.len() < 20 + 8 {
        return None;
    }
    let ts_bytes: [u8; 8] = key[16..24].try_into().ok()?;
    Some(u64::from_be_bytes(ts_bytes))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::store::MemoryEngine;

    #[test]
    fn audit_record_and_scan() {
        let store: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
        let log = KeyAuditLog::new(store);

        log.record(&KeyEvent::now(
            KeyEventKind::Created,
            "tenant-dek:acme",
            "DEK created for tenant acme",
        ))
        .unwrap();
        log.record(&KeyEvent::now(
            KeyEventKind::Used,
            "tenant-dek:acme",
            "first encrypt for field:salary",
        ))
        .unwrap();
        log.record(&KeyEvent::now(
            KeyEventKind::Rotated,
            "kek:master",
            "KEK rotated, 3 DEKs rewrapped",
        ))
        .unwrap();

        let all = log.scan(None, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].kind, KeyEventKind::Created);
        assert_eq!(all[1].kind, KeyEventKind::Used);
        assert_eq!(all[2].kind, KeyEventKind::Rotated);

        // Filter by label prefix
        let acme_only = log.scan(Some("tenant-dek:acme"), 100).unwrap();
        assert_eq!(acme_only.len(), 2);

        let counts = log.counts_by_kind().unwrap();
        assert_eq!(counts[0].1, 1); // Created=1
        assert_eq!(counts[1].1, 1); // Rotated=1
        assert_eq!(counts[2].1, 1); // Used=1
    }

    #[test]
    fn audit_event_encode_decode_roundtrip() {
        let ev = KeyEvent::now(
            KeyEventKind::Failure,
            "tenant-dek:corp",
            "decrypt failed: invalid tag",
        );
        assert_eq!(ev.kind, KeyEventKind::Failure);

        let (_, val) = ev.encode(0);
        let decoded = KeyEvent::decode(None, &val).unwrap();
        assert_eq!(decoded.kind, KeyEventKind::Failure);
        assert_eq!(decoded.key_label, "tenant-dek:corp");
        assert_eq!(decoded.detail, "decrypt failed: invalid tag");
    }

    #[test]
    fn audit_limit_truncates() {
        let store: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
        let log = KeyAuditLog::new(store);
        for i in 0..5 {
            log.record(&KeyEvent::now(
                KeyEventKind::Created,
                &format!("key-{}", i),
                "test",
            ))
            .unwrap();
        }
        let limited = log.scan(None, 3).unwrap();
        assert_eq!(limited.len(), 3);
    }
}
