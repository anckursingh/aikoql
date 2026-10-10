//! The F-05 sweep: every FZ-01..07 cargo-fuzz entry point runs over a
//! deterministic pseudo-random sweep in the DEFAULT build (no nightly, no
//! libFuzzer — rustc ships the engine only on Linux/macOS). The engine arm
//! (fuzz/smoke.sh) runs the same entry points with mutated inputs; this
//! keeps a plain-test check that they never crash.

use aikoql_storage_v2::fuzz::{
    check_checkpoint, check_current, check_directories, check_envelope_snapshot, check_manifest,
    check_wal_frame, check_wal_replay,
};

#[test]
fn seeds_and_sweep() {
    // One hostile sample per boundary — the checks' embedded valid inputs
    // cover the deep paths; these cover the garbage frontier.
    let hostile = [b"".to_vec(), vec![0xff; 64], vec![0x41; 33], vec![0; 1]];
    for h in &hostile {
        check_current(h);
        check_manifest(h);
        check_wal_frame(h);
        check_wal_replay(h);
        check_checkpoint(h);
        check_directories(h);
        check_envelope_snapshot(h);
    }

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
        check_current(&data);
        check_manifest(&data);
        check_wal_frame(&data);
        check_wal_replay(&data);
        check_checkpoint(&data);
        check_directories(&data);
        check_envelope_snapshot(&data);
    }
}
