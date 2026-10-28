//! MRFC-0080 R5a — automatic failover: one lease, one term.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ha-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn lease(holder: &str, term: u64, beat: u64) -> Lease {
        Lease {
            holder: holder.into(),
            term,
            repl: "127.0.0.1:1".into(),
            client: None,
            seq: 0,
            beat,
        }
    }

    /// The whole safety argument rests on this: a write lands only when
    /// the version it read is still current.
    #[test]
    fn a_file_lease_is_a_compare_and_swap() {
        let store = FileLease::new(tmp("cas").join("lease.json"));
        assert!(store.read().unwrap().is_none());
        let v1 = store.write(&lease("a", 1, 0), None).unwrap();
        assert!(
            matches!(
                store.write(&lease("b", 1, 0), None),
                Err(LeaseError::Conflict)
            ),
            "two creators won"
        );
        let (got, v) = store.read().unwrap().unwrap();
        assert_eq!((got.holder.as_str(), got.term), ("a", 1));
        assert_eq!(v, v1);
        let v2 = store.write(&lease("a", 1, 1), Some(&v1)).unwrap();
        assert_ne!(v1, v2, "a renewal must change the version");
        assert!(
            matches!(
                store.write(&lease("b", 2, 0), Some(&v1)),
                Err(LeaseError::Conflict)
            ),
            "a stale version won"
        );
    }

    #[test]
    fn concurrent_creators_have_exactly_one_winner() {
        let store = Arc::new(FileLease::new(tmp("race").join("lease.json")));
        let wins: usize = (0..8)
            .map(|i| {
                let s = store.clone();
                std::thread::spawn(move || s.write(&lease(&format!("n{i}"), 1, 0), None).is_ok())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap() as usize)
            .sum();
        assert_eq!(wins, 1);
    }
}
