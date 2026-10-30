//! AG-30 — the evidence pack's audit chain, signed with Ed25519 when the
//! server holds a key (AIKOQL_AUDIT_SIGNING_KEY: a file with the 32-byte
//! seed as hex), so anyone with the public key can check the export. A
//! configured but unusable key refuses to start: unsigned evidence from a
//! server meant to sign is worse than no signing.

use aikoql_kernel::security::SigningKey;
use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// RFC 8032 §7.1 TEST 1: a seed whose public key is a known value.
const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("audit-sig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn serve(root: &Path, key: Option<&Path>) -> std::process::Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"));
    cmd.arg("serve")
        .arg(root.join("db"))
        .env_remove("AIKOQL_AUDIT_SIGNING_KEY")
        .env("HOME", root.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(k) = key {
        cmd.env("AIKOQL_AUDIT_SIGNING_KEY", k);
    }
    cmd.spawn().expect("spawn aikoql-mcp serve")
}

/// initialize, one remember (so the chain is non-trivial), evidence_pack.
fn evidence_pack(child: &mut std::process::Child) -> J {
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let calls = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "remember", "arguments": {"type_name": "note", "properties": {"n": 1}}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "evidence_pack", "arguments": {}}}),
    ];
    for c in &calls {
        writeln!(stdin, "{c}").unwrap();
    }
    stdin.flush().unwrap();
    loop {
        let mut line = String::new();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "server closed stdout"
        );
        let Ok(frame) = serde_json::from_str::<J>(line.trim()) else {
            continue;
        };
        if frame["id"] == json!(3) {
            let text = frame["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("{frame}"));
            return serde_json::from_str(text).unwrap();
        }
    }
}

#[test]
fn as01_a_signing_server_signs_the_audit_chain() {
    let root = tmp("signed");
    let key = root.join("audit.key");
    std::fs::write(&key, format!("{SEED}\n")).unwrap();
    let mut child = serve(&root, Some(&key));
    let pack = evidence_pack(&mut child);
    let _ = child.kill();
    let sig = &pack["audit_signature"];
    assert_eq!(sig["algorithm"], json!("ed25519"), "{pack}");
    assert_eq!(
        sig["public_key"],
        json!(PUBLIC),
        "the key's RFC 8032 public key"
    );
    assert_eq!(sig["signed"], json!("audit_chain"));
    let chain = hex(pack["audit_chain"].as_str().unwrap());
    assert_eq!(chain.len(), 32);
    let public: [u8; 32] = hex(PUBLIC).try_into().unwrap();
    let signature: [u8; 64] = hex(sig["signature"].as_str().unwrap()).try_into().unwrap();
    assert!(
        SigningKey::verify(&public, &chain, &signature),
        "the signature verifies with the public key alone"
    );
}

#[test]
fn as02_no_key_no_signature() {
    let root = tmp("unsigned");
    let mut child = serve(&root, None);
    let pack = evidence_pack(&mut child);
    let _ = child.kill();
    assert!(pack["audit_chain"].is_string(), "{pack}");
    assert!(pack.get("audit_signature").is_none(), "{pack}");
}

#[test]
fn as03_an_unusable_key_refuses_to_start() {
    let root = tmp("bad-key");
    let key = root.join("audit.key");
    std::fs::write(&key, "not-a-hex-seed\n").unwrap();
    let mut child = serve(&root, Some(&key));
    let status = (0..100)
        .find_map(|_| {
            let s = child.try_wait().unwrap();
            if s.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            s
        })
        .unwrap_or_else(|| {
            let _ = child.kill();
            panic!("a server with an unusable signing key kept running")
        });
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert_eq!(status.code(), Some(2), "{err}");
    assert!(err.contains("AIKOQL_AUDIT_SIGNING_KEY"), "{err}");
}
