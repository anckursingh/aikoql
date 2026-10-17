//! The §15 smoke: every cargo-fuzz entry point runs over the real-wire
//! seed set plus a deterministic pseudo-random sweep in the DEFAULT build
//! (no nightly, no libFuzzer — rustc ships the engine only on Linux/macOS).
//! The engine arm (fuzz/smoke.sh) runs the same entry points with mutated
//! inputs; this keeps a plain-test check that they never crash.

use aikoql_sdk::fuzz::{
    check_auth_frame, check_error_frame, check_native_frame, check_protocol_version,
    check_request_decoder, check_response_decoder, check_rpc_frame, check_stream_frame,
};

#[test]
fn seeds_and_sweep() {
    run(check_rpc_frame, &[
        br#"{"jsonrpc":"2.0","id":1,"result":{"koid":"x"}}"#.to_vec(),
        br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"nf"}}"#.to_vec(),
        br#"{"jsonrpc":"2.0","method":"notifications/notify","params":{"stream_id":"s","done":true}}"#.to_vec(),
        br#"{}"#.to_vec(),
        b"not json".to_vec(),
    ]);
    run(
        check_error_frame,
        &[
            br#"{"code":-32601,"message":"method not found"}"#.to_vec(),
            br#"{"code":"-32601","message":"string-encoded"}"#.to_vec(),
            br#"{"code":"FRAME_TOO_LARGE","message":"over cap"}"#.to_vec(),
            br#"{"code":null,"message":""}"#.to_vec(),
            br#"{"message":"no code"}"#.to_vec(),
        ],
    );
    run(
        check_stream_frame,
        &[
            br#"{"method":"notifications/notify","params":{"stream_id":"s1","done":true}}"#
                .to_vec(),
            br#"{"method":"notifications/notify","params":{"stream_id":"s1"}}"#.to_vec(),
            br#"{"method":"notifications/notify","params":{"done":true}}"#.to_vec(),
            br#"{}"#.to_vec(),
            br#"{"method":"x","params":{"stream_id":"s1"}}"#.to_vec(),
        ],
    );
    run(
        check_protocol_version,
        &[
            b"0.2.0".to_vec(),
            b"0.1.19".to_vec(),
            b"2024-11-05".to_vec(),
            b"x.y".to_vec(),
            b"1.2.3.4.5".to_vec(),
            b"".to_vec(),
        ],
    );
    run(
        check_request_decoder,
        &[
            longs(1, 1),
            longs(1, 2),
            longs(2, 1),
            longs(0, 0),
            longs(u64::MAX, 5),
        ],
    );
    run(
        check_response_decoder,
        &[
            pack(1, br#"{"id":1,"result":{}}"#),
            pack(2, br#"{"id":1,"result":{}}"#),
            pack(1, br#"{"id":2,"result":{}}"#),
            pack(1, br#"{"id":1,"error":{"code":-32601,"message":"nf"}}"#),
            pack(1, br#"{"id":"x","result":{}}"#),
            pack(0, b"garbage"),
        ],
    );
    run(
        check_native_frame,
        &[
            vec![],
            b"AKQL\x00\x01\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x09\x00\x00\x00\x03{}"
                .to_vec(),
            vec![0u8; 22],
        ],
    );
    run(
        check_auth_frame,
        &[
            br#"{"code":"-32001","message":"m"}"#.to_vec(),
            br#"{"code":"FRAME_TOO_LARGE","message":"over"}"#.to_vec(),
            br#"{"code":123,"message":"n"}"#.to_vec(),
            br#"{}"#.to_vec(),
            b"garbage".to_vec(),
        ],
    );
    sweep();
}

/// A deterministic pseudo-random sweep — every entry point over 2000
/// inputs, so a crash surfaces without the engine.
fn sweep() {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..2000 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let len = ((x >> 33) % 49) as usize;
        let mut data = vec![0u8; len];
        for b in data.iter_mut() {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (x >> 56) as u8;
        }
        check_rpc_frame(&data);
        check_native_frame(&data);
        check_error_frame(&data);
        check_stream_frame(&data);
        check_protocol_version(&data);
        check_request_decoder(&data);
        check_response_decoder(&data);
        check_auth_frame(&data);
    }
}

fn run(target: fn(&[u8]), seeds: &[Vec<u8>]) {
    for seed in seeds {
        target(seed);
    }
}

/// Two big-endian ids in one 16-byte input.
fn longs(want: u64, got: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&want.to_be_bytes());
    out.extend_from_slice(&got.to_be_bytes());
    out
}

/// A request id followed by the response line (the check's framing).
fn pack(want: u64, line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + line.len());
    out.extend_from_slice(&want.to_be_bytes());
    out.extend_from_slice(line);
    out
}
