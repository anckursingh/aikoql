//! The §16 pin: the cross-language golden corpus spec exists at
//! sdk-fuzz-corpus/corpus.json and this SDK's column holds for every case.
//! Removing a case id is a detected coverage loss; a column mismatch is a
//! wire-behavior drift (or an undocumented divergence — document it in the
//! spec's note and re-stamp). The evaluation runs the real shared wire
//! functions (the same `fuzz::` surface the §15 targets exercise), so a
//! drift here is a client drift, not a test artifact.

use aikoql_sdk::fuzz::{
    classify_id, decode_notify, decode_response, parse_version, Corr, RpcResponse,
};
use serde_json::{json, Value};

const CASE_IDS: [&str; 20] = [
    "corp-v01", "corp-v02", "corp-v03", "corp-v04", "corp-v05", "corp-e01", "corp-e02", "corp-e03",
    "corp-e04", "corp-e05", "corp-r01", "corp-r02", "corp-n01", "corp-s01", "corp-s02", "corp-s03",
    "corp-c01", "corp-c02", "corp-c03", "corp-c04",
];
const SURFACES: [&str; 6] = [
    "version",
    "rpc_error",
    "response_id",
    "nonfinite",
    "notify",
    "correlation",
];

/// The §16 stream the notify verdicts run against (the client loop's
/// stream id).
const STREAM_ID: &str = "s1";

fn corr_name(c: Corr) -> &'static str {
    match c {
        Corr::Skip => "skip",
        Corr::Match => "match",
        Corr::Protocol => "protocol",
    }
}

/// This SDK's classification of one corpus input, as a JSON value.
fn evaluate(surface: &str, input: &Value) -> Value {
    match surface {
        "version" => {
            let v = input.as_str().expect("version input is a string");
            let parts: Vec<Value> = parse_version(v).into_iter().map(Value::from).collect();
            Value::Array(parts)
        }
        "rpc_error" => {
            let frame = input.as_str().expect("rpc_error input is a string");
            match decode_response(frame) {
                Some((_, Some(me), _)) => json!({"code": me.code, "message": me.message}),
                // The frame fails the struct parse — noise skip.
                _ => Value::String("reject".into()),
            }
        }
        "response_id" | "nonfinite" => {
            let frame = input.as_str().expect("frame input is a string");
            match decode_response(frame) {
                Some((rid, _, _)) => Value::String(corr_name(classify_id(1, rid)).into()),
                None => Value::String("skip".into()),
            }
        }
        "notify" => {
            let frame = input.as_str().expect("notify input is a string");
            let resp: Option<RpcResponse> = serde_json::from_str(frame).ok();
            match resp.as_ref().and_then(decode_notify) {
                Some((sid, done)) if sid == STREAM_ID => json!({
                    "verdict": "yield",
                    "pair": {"stream_id": sid, "done": done},
                }),
                _ => json!({"verdict": "skip", "pair": null}),
            }
        }
        "correlation" => {
            let want = input["want"].as_u64().expect("want is a number");
            let got = input["got"].as_u64().expect("got is a number");
            Value::String(corr_name(classify_id(want, got)).into())
        }
        other => panic!("unknown surface {other}"),
    }
}

#[test]
fn corpus_pin() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let spec_path = manifest
        .join("../../..")
        .join("sdk-fuzz-corpus")
        .join("corpus.json");
    let raw = std::fs::read_to_string(&spec_path).unwrap_or_else(|e| {
        panic!(
            "the §16 corpus is absent ({}): the D-16 golden-corpus slice is missing — {e}",
            spec_path.display()
        )
    });
    let spec: Value = serde_json::from_str(&raw).expect("the §16 corpus does not parse");
    let cases = spec["cases"]
        .as_array()
        .expect("the §16 corpus has no cases array");

    let ids: Vec<&str> = cases
        .iter()
        .map(|c| c["id"].as_str().unwrap_or(""))
        .collect();
    for id in CASE_IDS {
        assert!(
            ids.contains(&id),
            "case {id} is gone from the §16 corpus — a removed case is a coverage loss"
        );
    }
    let surfaces: Vec<&str> = cases
        .iter()
        .map(|c| c["surface"].as_str().unwrap_or(""))
        .collect();
    for s in SURFACES {
        assert!(
            surfaces.contains(&s),
            "surface {s} has no cases in the §16 corpus"
        );
    }

    for c in cases {
        let id = c["id"].as_str().unwrap_or("");
        let surface = c["surface"].as_str().unwrap_or("");
        let want = c["expected"]
            .get("rust")
            .unwrap_or_else(|| panic!("case {id} has no rust column in the §16 corpus"));
        let got = evaluate(surface, &c["input"]);
        assert_eq!(
            got, *want,
            "case {id} ({surface}): rust column drift — got {got}, want {want}"
        );
    }
}
