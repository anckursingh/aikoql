//! The D-16 fuzz surface (§15): the pure wire logic the client runs, in
//! functions shared with the cargo-fuzz targets. The fuzz crate is a
//! separate, non-workspace package (its own `[workspace]` table) that can
//! only reach `pub` API — hence this doc(hidden) module. The eight
//! `check_*` entry points are the §15 targets' bodies: each restates a
//! frozen wire property over the real shared logic and panics on violation
//! (a crash to libFuzzer). Valid frames are embedded in the checks so the
//! engine reaches the deep paths without a seed corpus.

use crate::error::McpError;
use aikoql_native as nat;
use serde::Deserialize;
use serde_json::Value;

/// One MCP JSON-RPC response frame (the wire shape the client reads).
#[derive(Deserialize, Clone)]
pub struct RpcResponse {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// An RPC-level error frame: the code is any JSON value; string-encoded
/// codes and numbers both normalize to their string form.
#[derive(Deserialize, Clone)]
pub struct RpcError {
    pub code: Value,
    pub message: String,
}

impl RpcError {
    pub(crate) fn mcp_error(self) -> McpError {
        map_rpc_error(&self.code, &self.message)
    }
}

/// A stream notify chunk (serde defaults: a missing stream_id is "", a
/// missing done is false).
#[derive(Deserialize, Clone)]
struct NotifyChunk {
    #[serde(default)]
    stream_id: String,
    #[serde(default)]
    done: bool,
}

/// The frozen RPC-error mapping: the code's string form (empty → INTERNAL),
/// the message verbatim. The Rust SDK keeps `.message` raw — only `Display`
/// decorates it with `[code]` (a §16 divergence: TS/Java decorate the
/// message itself, Go/Python/Rust keep it raw).
pub fn map_rpc_error(code: &Value, message: &str) -> McpError {
    let code = match code {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    };
    let code = if code.is_empty() {
        "INTERNAL".into()
    } else {
        code
    };
    McpError {
        code,
        message: message.to_string(),
        retryable: false,
        suggestion: String::new(),
    }
}

/// Parses one response line into the request loop's verdict: (id, mapped
/// error, result). A frame that fails the struct parse (garbage, or a
/// non-numeric id — the frozen §3.3 skip: tolerated as noise, like the TS
/// SDK) is None.
pub fn decode_response(data: &str) -> Option<(u64, Option<McpError>, Option<Value>)> {
    let resp: RpcResponse = serde_json::from_str(data.trim()).ok()?;
    Some((
        resp.id.unwrap_or(0),
        resp.error.map(|e| e.mcp_error()),
        resp.result,
    ))
}

/// The frozen notify guard: a frame is a stream notify only when its
/// method is notifications/notify and its params carry the chunk shape.
/// Returns the (stream_id, done) pair — a missing stream_id decodes to ""
/// (serde default).
pub fn decode_notify(resp: &RpcResponse) -> Option<(String, bool)> {
    if resp.method.as_deref() != Some("notifications/notify") {
        return None;
    }
    let p: NotifyChunk = serde_json::from_value(resp.params.clone().unwrap_or(Value::Null)).ok()?;
    Some((p.stream_id, p.done))
}

/// The frozen §3.3 correlation rules (the Go corr* constants), restated
/// independently: smaller ids skip, larger ids are PROTOCOL_ERROR, equal
/// ids match.
#[derive(Debug, PartialEq, Eq)]
pub enum Corr {
    Skip,
    Protocol,
    Match,
}

pub fn classify_id(want: u64, got: u64) -> Corr {
    if got < want {
        Corr::Skip
    } else if got > want {
        Corr::Protocol
    } else {
        Corr::Match
    }
}

/// An ERROR frame payload → McpError. The server sends the native codes
/// directly; the legacy "-32001" spelling (the MCP auth vector's frozen
/// expectation) maps to AUTHENTICATION_FAILED. A missing or non-string
/// code is INTERNAL, a missing or non-string message is "".
pub fn map_native_error(payload: &[u8]) -> McpError {
    let env: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
    let code = env
        .get("code")
        .and_then(|c| c.as_str())
        .map(|c| {
            if c == "-32001" {
                "AUTHENTICATION_FAILED"
            } else {
                c
            }
        })
        .unwrap_or("INTERNAL")
        .to_string();
    let message = env
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    McpError {
        code,
        message,
        retryable: false,
        suggestion: String::new(),
    }
}

/// Mirrors the Go SDK's dotted-int tuple: non-numeric segments become -1
/// (never >=). Agrees with Python's int(seg) — "0x10" and "" both refuse
/// (the TS SDK's Number() accepted them; the D-16 slice 5 fix).
pub fn parse_version(v: &str) -> Vec<i64> {
    v.split('.')
        .map(|seg| seg.parse::<i64>().unwrap_or(-1))
        .collect()
}

/// Compares two dotted version tuples segment by segment.
pub fn version_less(a: &[i64], b: &[i64]) -> bool {
    for i in 0..a.len().min(b.len()) {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    a.len() < b.len()
}

// — §15 target bodies: fuzz_rpc_frame, fuzz_native_frame, fuzz_error_frame,
// fuzz_stream_frame, fuzz_protocol_version, fuzz_request_decoder,
// fuzz_response_decoder, fuzz_auth_frame. —

fn same_response(
    a: &Option<(u64, Option<McpError>, Option<Value>)>,
    b: &Option<(u64, Option<McpError>, Option<Value>)>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some((ia, ea, ra)), Some((ib, eb, rb))) => {
            ia == ib
                && ea.as_ref().map(|e| (&e.code, &e.message))
                    == eb.as_ref().map(|e| (&e.code, &e.message))
                && ra == rb
        }
        _ => false,
    }
}

pub fn check_rpc_frame(data: &[u8]) {
    let line = String::from_utf8_lossy(data);
    let a = decode_response(&line);
    let b = decode_response(&line);
    assert!(
        same_response(&a, &b),
        "response decode is not deterministic"
    );
    if let Some((_, Some(err), _)) = &a {
        assert!(!err.code.is_empty(), "mapped error code is empty");
    }
    // The frozen deep paths, embedded so the engine reaches them.
    let ok = decode_response(r#"{"jsonrpc":"2.0","id":1,"result":{"koid":"x"}}"#);
    assert!(matches!(&ok, Some((1, None, Some(Value::Object(_))))));
    let err = decode_response(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"nf"}}"#);
    match err {
        Some((2, Some(e), _)) => {
            assert_eq!(e.code, "-32601");
            assert_eq!(e.message, "nf");
        }
        other => panic!("error frame did not decode: {other:?}"),
    }
}

pub fn check_native_frame(data: &[u8]) {
    // Zero-pad to a header — the codec must never panic on arbitrary bytes.
    let mut header = [0u8; nat::HEADER_LEN];
    let n = data.len().min(nat::HEADER_LEN);
    header[..n].copy_from_slice(&data[..n]);
    let a = nat::parse_header(&header);
    let b = nat::parse_header(&header);
    assert_eq!(a, b, "header decode is not deterministic");
    if let Ok(h) = a {
        // The round-trip: the parsed header rebuilds to its own bytes.
        let rebuilt = nat::header_bytes(h.flags, h.request_id, h.msg_type, h.payload_len as u32);
        assert_eq!(rebuilt, header, "header does not rebuild to its own bytes");
        // A claimed frame whose payload + checksum are present must verify
        // exactly when the checksum matches.
        let end = nat::HEADER_LEN + h.payload_len + 4;
        if let Some(frame) = data.get(..end.min(data.len())) {
            if frame.len() == end {
                let payload = &frame[nat::HEADER_LEN..nat::HEADER_LEN + h.payload_len];
                let crc = u32::from_le_bytes(
                    frame[end - 4..end].try_into().expect("4 bytes"), // justified: fixed slice
                );
                let want = nat::crc32(&frame[..end - 4]) == crc;
                assert_eq!(nat::verify(&header, payload, crc), want);
            }
        }
    }
    // Embedded: a well-formed frame verifies; a bad magic and an over-cap
    // claim are rejected before a payload byte is read.
    let good_header = nat::header_bytes(nat::FLAG_RESPONSE, 1, nat::QUERY, 3);
    let mut good = good_header.to_vec();
    good.extend_from_slice(b"{}");
    let crc = nat::crc32(&good);
    good.extend_from_slice(&crc.to_le_bytes());
    assert_eq!(
        nat::verify(&good_header, b"{}", crc),
        true,
        "a well-formed frame must verify"
    );
    let bad = [0u8; nat::HEADER_LEN];
    assert_eq!(nat::parse_header(&bad), Err(nat::DecodeError::BadMagic));
    let mut oversize = good_header;
    oversize[18..22].copy_from_slice(&u32::MAX.to_be_bytes());
    match nat::parse_header(&oversize) {
        Err(nat::DecodeError::Oversized { claimed }) => {
            assert_eq!(claimed, u32::MAX as usize);
        }
        other => panic!("over-cap header did not reject: {other:?}"),
    }
}

pub fn check_error_frame(data: &[u8]) {
    let err: RpcError = match serde_json::from_slice(data) {
        Ok(e) => e,
        Err(_) => return, // garbage cannot arrive at the mapping
    };
    let me = err.clone().mcp_error();
    let again = err.clone().mcp_error();
    assert!(!me.code.is_empty(), "mapped code is empty");
    assert_eq!(
        me.message, err.message,
        "the message must pass through verbatim — the Rust SDK keeps .message raw (only Display decorates)"
    );
    assert_eq!(
        (me.code, me.message),
        (again.code, again.message),
        "mapping is not deterministic"
    );
    // Embedded frozen truths: string codes decode as-is, the empty code is
    // INTERNAL, and a non-string code maps to its JSON string form.
    let cases: &[(&[u8], &str)] = &[
        (
            br#"{"code":"-32601","message":"method not found"}"#,
            "-32601",
        ),
        (br#"{"code":"","message":"m"}"#, "INTERNAL"),
        (br#"{"code":null,"message":""}"#, "null"),
    ];
    for (frame, want) in cases {
        let e: RpcError = serde_json::from_slice(frame).expect("embedded frame parses");
        assert_eq!(
            e.clone().mcp_error().code,
            *want,
            "frozen code mapping drifted"
        );
    }
}

pub fn check_stream_frame(data: &[u8]) {
    let resp: RpcResponse = match serde_json::from_slice(data) {
        Ok(r) => r,
        Err(_) => return,
    };
    let first = decode_notify(&resp);
    let second = decode_notify(&resp);
    assert_eq!(first, second, "notify decode is not deterministic");
    if let Some((sid, done)) = &first {
        // The round-trip: the decoded pair re-encodes to the same pair.
        let frame = serde_json::to_vec(&serde_json::json!({
            "method": "notifications/notify",
            "params": {"stream_id": sid, "done": done},
        }))
        .expect("json! is infallible");
        let again: RpcResponse = serde_json::from_slice(&frame).expect("round-trip parses");
        assert_eq!(decode_notify(&again).as_ref(), Some(&(sid.clone(), *done)));
    }
    // Embedded: the notify shapes decode to their pairs; anything else is
    // None; a missing stream_id decodes to "" (the serde default).
    let cases: &[(&[u8], Option<(&str, bool)>)] = &[
        (
            br#"{"method":"notifications/notify","params":{"stream_id":"s1","done":true}}"#,
            Some(("s1", true)),
        ),
        (
            br#"{"method":"notifications/notify","params":{"stream_id":"s1"}}"#,
            Some(("s1", false)),
        ),
        (
            br#"{"method":"notifications/notify","params":{"done":true}}"#,
            Some(("", true)),
        ),
        (br#"{}"#, None),
        (br#"{"method":"notifications/notify"}"#, None),
    ];
    for (frame, want) in cases {
        let r: RpcResponse = serde_json::from_slice(frame).expect("embedded frame parses");
        let got = decode_notify(&r).map(|(s, d)| (s, d));
        assert_eq!(
            got.as_ref().map(|(s, d)| (s.as_str(), *d)),
            *want,
            "frozen notify decode drifted"
        );
    }
}

pub fn check_protocol_version(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let a = parse_version(&s);
    let b = parse_version(&s);
    assert_eq!(a, b, "version parse is not deterministic");
    assert!(!version_less(&a, &a), "version_less must be irreflexive");
    // The frozen int(seg) mirror: a non-numeric segment is -1, never more.
    for (seg, val) in s.split('.').zip(a.iter()) {
        if seg.parse::<i64>().is_err() {
            assert_eq!(*val, -1, "non-numeric segment {seg:?} mapped to {val}");
        }
    }
    // Embedded: the dotted parse + comparison truths the TS slice diverged on.
    assert_eq!(parse_version("0.2.0"), vec![0, 2, 0]);
    assert_eq!(parse_version("0x10"), vec![-1]);
    assert_eq!(parse_version(""), vec![-1]);
    assert_eq!(parse_version("1.2.3.4.5"), vec![1, 2, 3, 4, 5]);
    assert!(version_less(&[0, 1, 19], &[0, 2, 0]));
    assert!(!version_less(&[0, 2, 0], &[0, 1, 19]));
}

pub fn check_request_decoder(data: &[u8]) {
    // Two big-endian ids per input (zero-fill for short ones, like the
    // Java slice) — negatives are reachable as large unsigned ids.
    let want = u64::from_be_bytes(
        data.get(..8)
            .map(|s| {
                let mut b = [0u8; 8];
                b[..s.len()].copy_from_slice(s);
                b
            })
            .unwrap_or([0u8; 8]),
    );
    let got = u64::from_be_bytes(
        data.get(8..16)
            .map(|s| {
                let mut b = [0u8; 8];
                b[..s.len()].copy_from_slice(s);
                b
            })
            .unwrap_or([0u8; 8]),
    );
    let want_corr = if got < want {
        Corr::Skip
    } else if got > want {
        Corr::Protocol
    } else {
        Corr::Match
    };
    assert_eq!(
        classify_id(want, got),
        want_corr,
        "correlation rules drifted"
    );
    // Embedded: the frozen §3.3 pairs.
    assert_eq!(classify_id(1, 1), Corr::Match);
    assert_eq!(classify_id(1, 2), Corr::Protocol);
    assert_eq!(classify_id(2, 1), Corr::Skip);
    assert_eq!(classify_id(0, 0), Corr::Match);
    assert_eq!(classify_id(u64::MAX, 5), Corr::Skip);
}

pub fn check_response_decoder(data: &[u8]) {
    // First eight bytes = the request id, the rest = the response line.
    let want = u64::from_be_bytes(
        data.get(..8)
            .map(|s| {
                let mut b = [0u8; 8];
                b[..s.len()].copy_from_slice(s);
                b
            })
            .unwrap_or([0u8; 8]),
    );
    let line = String::from_utf8_lossy(data.get(8..).unwrap_or(&[]));
    let a = decode_response(&line);
    let b = decode_response(&line);
    assert!(
        same_response(&a, &b),
        "response decode is not deterministic"
    );
    if let Some((rid, err, _)) = &a {
        if let Some(e) = err {
            assert!(!e.code.is_empty(), "mapped error code is empty");
        }
        // The request loop's verdict for this id.
        assert_eq!(
            classify_id(want, *rid),
            if *rid < want {
                Corr::Skip
            } else if *rid > want {
                Corr::Protocol
            } else {
                Corr::Match
            },
            "the loop verdict drifted from the frozen rules"
        );
    }
    // Embedded: the loop's verdicts — a late frame skips, a future id is a
    // PROTOCOL_ERROR, and a non-numeric id fails the struct parse (the
    // frozen noise skip, TS parity — never a protocol error).
    let late = decode_response(r#"{"id":1,"result":{}}"#);
    assert!(matches!(late, Some((1, None, Some(_)))));
    assert_eq!(classify_id(2, 1), Corr::Skip);
    assert_eq!(classify_id(1, 2), Corr::Protocol);
    assert!(decode_response(r#"{"id":"x","result":{}}"#).is_none());
}

pub fn check_auth_frame(data: &[u8]) {
    let a = map_native_error(data);
    let b = map_native_error(data);
    assert!(!a.code.is_empty(), "mapped native code is empty");
    assert_eq!(
        (a.code, a.message),
        (b.code, b.message),
        "native error mapping is not deterministic"
    );
    // Embedded: the frozen native mapping — the legacy -32001 spelling is
    // AUTHENTICATION_FAILED, other string codes pass as-is, and a missing
    // or non-string code is INTERNAL.
    let cases: &[(&[u8], &str, &str)] = &[
        (
            br#"{"code":"-32001","message":"m"}"#,
            "AUTHENTICATION_FAILED",
            "m",
        ),
        (
            br#"{"code":"FRAME_TOO_LARGE","message":"over"}"#,
            "FRAME_TOO_LARGE",
            "over",
        ),
        (br#"{"code":123,"message":"n"}"#, "INTERNAL", "n"),
        (br#"{}"#, "INTERNAL", ""),
    ];
    for (frame, want_code, want_msg) in cases {
        let m = map_native_error(frame);
        assert_eq!(
            (m.code.as_str(), m.message.as_str()),
            (*want_code, *want_msg)
        );
    }
}
