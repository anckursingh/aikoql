//! MRFC-0080 R3a — the replication stream: a leader serves its storage
//! log over TCP to standbys that apply it with `apply_replicated`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TcpAuthTable;
    use aikoql_storage_v2::db::{Config, Db};
    use aikoql_storage_v2::engine::StorageAdminApi;
    use aikoql_storage_v2::AikoqlStorageEngineV2;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "repl-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    struct Leader {
        engine: Arc<AikoqlStorageEngineV2>,
        addr: String,
        dir: PathBuf,
        stop: Arc<AtomicBool>,
    }

    fn leader(tag: &str) -> Leader {
        let dir = tmp(tag);
        let mut cfg = Config::new(dir.clone());
        cfg.retain_log = true;
        cfg.memtable_bytes = 1024; // the stream crosses flush truncations
        let engine = Arc::new(AikoqlStorageEngineV2::open_with_config(cfg).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let auth = Arc::new(
            TcpAuthTable::parse(&["leader-tok::admin".into(), "user-tok::viewer".into()]).unwrap(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let source: Arc<dyn StorageAdminApi> = engine.clone();
        let s = stop.clone();
        std::thread::spawn(move || serve_replication(listener, source, auth, s));
        Leader {
            engine,
            addr,
            dir,
            stop,
        }
    }

    fn write(l: &Leader, from: u32, to: u32) {
        for i in from..to {
            l.engine
                .db()
                .put(format!("k{i:04}").as_bytes(), &[i as u8; 64])
                .unwrap();
        }
    }

    fn wait_for(cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond() {
            assert!(Instant::now() < deadline, "replication did not converge");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn standby_streams_catches_up_and_resumes_after_disconnect() {
        let l = leader("stream");
        write(&l, 0, 100);
        let standby = Arc::new(Db::open(Config::new(tmp("stream-standby"))).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let run = |stop: Arc<AtomicBool>| {
            let (addr, sb) = (l.addr.clone(), standby.clone());
            std::thread::spawn(move || follow(&addr, "leader-tok", &sb, &stop))
        };
        let h = run(stop.clone());
        write(&l, 100, 200); // live writes while streaming
        wait_for(|| standby.last_seq() == l.engine.db().last_seq());
        assert_eq!(standby.scan(b"").unwrap(), l.engine.db().scan(b"").unwrap());

        // the leader released what the standby acked
        let rlog = std::fs::read_dir(l.dir.join("rlog")).unwrap().count();
        assert!(
            rlog <= 1,
            "acked archive files were not released ({rlog} left)"
        );

        stop.store(true, Ordering::SeqCst);
        h.join().unwrap().unwrap();
        write(&l, 200, 300); // written while the standby is away
        let stop2 = Arc::new(AtomicBool::new(false));
        let h2 = run(stop2.clone());
        wait_for(|| standby.last_seq() == l.engine.db().last_seq());
        assert_eq!(standby.scan(b"").unwrap(), l.engine.db().scan(b"").unwrap());
        stop2.store(true, Ordering::SeqCst);
        h2.join().unwrap().unwrap();
        l.stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn non_admin_and_unknown_tokens_are_refused() {
        let l = leader("auth");
        write(&l, 0, 10);
        let standby = Db::open(Config::new(tmp("auth-standby"))).unwrap();
        let stop = AtomicBool::new(false);
        for tok in ["user-tok", "nope"] {
            let err = follow(&l.addr, tok, &standby, &stop).unwrap_err();
            assert!(err.contains("refused"), "{tok}: {err}");
        }
        assert_eq!(
            standby.last_seq(),
            0,
            "nothing streamed to a refused standby"
        );
        l.stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_standby_behind_the_retained_log_is_told_to_rebootstrap() {
        let mut cfg = Config::new(tmp("gap"));
        cfg.memtable_bytes = 1024; // no retain_log: flushed history is gone
        let engine = Arc::new(AikoqlStorageEngineV2::open_with_config(cfg).unwrap());
        for i in 0..100u32 {
            engine
                .db()
                .put(format!("k{i}").as_bytes(), &[0u8; 64])
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let auth = Arc::new(TcpAuthTable::parse(&["t::admin".into()]).unwrap());
        let source: Arc<dyn StorageAdminApi> = engine.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || serve_replication(listener, source, auth, s));

        let standby = Db::open(Config::new(tmp("gap-standby"))).unwrap();
        let err = follow(&addr, "t", &standby, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("re-bootstrap"), "{err}");
        stop.store(true, Ordering::SeqCst);
    }
}
