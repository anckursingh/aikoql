//! AG-21 — scan_from seeks (sf001–002). The kernel's journal replay
//! (`journal_after`: the index tail patch, AG-20's unindexed tail) calls
//! StorageEngine::scan_from, documented as a seek — but no engine
//! overrode the trait default, which scans the WHOLE prefix and filters:
//! every tail read was O(journal). The observable is the engine's own
//! decode counter (read_path_stats().entries_decoded).
mod common;
use aikoql_kernel::storage::store::StorageEngine;
use aikoql_kernel::WriteBatch;
use aikoql_storage_v2::AikoqlStorageEngineV2;
use common::dir;

const N: usize = 20_000;

fn key(i: usize) -> Vec<u8> {
    format!("e/{i:08}").into_bytes()
}

fn seeded(tag: &str) -> AikoqlStorageEngineV2 {
    let e = AikoqlStorageEngineV2::open(dir(tag)).unwrap();
    for chunk in (0..N).collect::<Vec<_>>().chunks(1000) {
        let mut b = WriteBatch::default();
        for &i in chunk {
            b.puts.push((key(i), vec![b'v'; 32]));
        }
        e.write_batch(&b).unwrap();
    }
    e.write_batch(&WriteBatch {
        puts: vec![(b"f/after".to_vec(), b"x".to_vec())],
        dels: vec![],
    })
    .unwrap();
    e.db().flush().unwrap();
    e
}

/// RED pre-fix: the tail of a 20 000-key prefix decoded every entry.
#[test]
fn sf001_scan_from_seeks_instead_of_scanning_the_prefix() {
    let e = seeded("sf001");
    let before = e.db().read_path_stats().entries_decoded;
    let rows = e.scan_from(b"e/", &key(N - 10)).unwrap();
    let decoded = e.db().read_path_stats().entries_decoded - before;
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0].0, key(N - 10));
    assert!(
        decoded <= 200,
        "the tail of {N} keys decoded {decoded} entries"
    );
}

/// Guard: scan_from == scan filtered to keys >= start, from every layer
/// (segments, then newer memtable puts, a delete shadowing a segment key).
#[test]
fn sf002_scan_from_equals_the_filtered_scan_across_layers() {
    let e = seeded("sf002");
    e.write_batch(&WriteBatch {
        puts: vec![
            (key(N + 1), b"new".to_vec()),
            (key(N - 3), b"over".to_vec()),
        ],
        dels: vec![key(N - 2)],
    })
    .unwrap();
    for start in [
        key(0),
        key(N - 5),
        key(N + 1),
        b"e/~".to_vec(),
        b"e/".to_vec(),
    ] {
        let want: Vec<_> = e
            .scan(b"e/")
            .unwrap()
            .into_iter()
            .filter(|(k, _)| *k >= start)
            .collect();
        assert_eq!(e.scan_from(b"e/", &start).unwrap(), want);
    }
}
