//! MRFC-0080 R6 — the continuous archive and point-in-time restore.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::Acked;
    use aikoql_storage_v2::db::{Config, Db};
    use aikoql_storage_v2::engine::StorageAdminApi;
    use aikoql_storage_v2::AikoqlStorageEngineV2;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "archive-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn engine(tag: &str) -> Arc<AikoqlStorageEngineV2> {
        engine_at(tmp(tag))
    }

    fn engine_at(dir: PathBuf) -> Arc<AikoqlStorageEngineV2> {
        let mut cfg = Config::new(dir);
        cfg.retain_log = true;
        cfg.memtable_bytes = 1024; // flushes: the archive reads across truncations
        Arc::new(AikoqlStorageEngineV2::open_with_config(cfg).unwrap())
    }

    fn put(e: &AikoqlStorageEngineV2, from: u32, to: u32) {
        for i in from..to {
            e.db()
                .put(format!("k{i:04}").as_bytes(), &[i as u8; 64])
                .unwrap();
        }
    }

    fn rows(dir: &std::path::Path) -> Vec<(Vec<u8>, Vec<u8>)> {
        Db::open(Config::new(dir.to_path_buf()))
            .unwrap()
            .scan(b"")
            .unwrap()
    }

    fn store_contract(s: &dyn ObjectStore) {
        s.put("a/1", b"one").unwrap();
        s.put("a/2", b"two").unwrap();
        s.put("b/1", b"other").unwrap();
        assert_eq!(s.list("a/").unwrap(), ["a/1", "a/2"]);
        assert_eq!(s.get("a/2").unwrap(), b"two");
        assert!(
            s.get("a/3").is_err(),
            "a missing key must not read as empty"
        );
        s.put("a/2", b"two again").unwrap();
        assert_eq!(s.get("a/2").unwrap(), b"two again");
    }

    #[test]
    fn a_dir_store_keeps_keys() {
        store_contract(&DirStore::new(tmp("dirstore")));
    }

    #[test]
    fn an_s3_store_keeps_keys_under_its_prefix() {
        let (endpoint, keys) = fake_s3();
        let s3 = crate::s3::S3::new(
            Some(endpoint),
            "bkt".into(),
            "us-east-1".into(),
            crate::s3::Creds {
                akid: "AKIDTEST".into(),
                secret: "secret".into(),
                token: None,
            },
            Duration::from_secs(5),
        );
        store_contract(&S3Store::new(s3, "cluster-1".into()));
        let mut stored: Vec<String> = keys.lock().unwrap().keys().cloned().collect();
        stored.sort();
        assert_eq!(stored, ["cluster-1/a/1", "cluster-1/a/2", "cluster-1/b/1"]);
    }

    #[test]
    fn list_xml_parses_keys_and_the_continuation() {
        let xml = "<ListBucketResult><IsTruncated>true</IsTruncated>\
            <Contents><Key>p/a&amp;b</Key></Contents><Contents><Key>p/c</Key></Contents>\
            <NextContinuationToken>tok/1=</NextContinuationToken></ListBucketResult>";
        assert_eq!(
            parse_list(xml),
            (
                vec!["p/a&b".to_string(), "p/c".to_string()],
                Some("tok/1=".to_string())
            )
        );
        let last = "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>";
        assert_eq!(parse_list(last), (vec![], None));
    }

    /// The round trip: archive a live db (base snapshot, then log passes
    /// across flush truncations), restore to the latest seq and to a seq
    /// in the middle — each byte-equal to the db at that seq.
    #[test]
    fn archive_then_restore_to_the_latest_and_to_a_seq() {
        let e = engine("rt");
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("rt-store")));
        let acked = Arc::new(Acked::default());
        put(&e, 0, 50);
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let mut a =
            Archiver::start(store.clone(), source, acked.clone(), 1, tmp("rt-scratch")).unwrap();
        put(&e, 50, 100);
        assert_eq!(a.pass().unwrap(), e.db().last_seq());
        let (mid, mid_rows) = (e.db().last_seq(), e.db().scan(b"").unwrap());
        put(&e, 100, 150);
        assert_eq!(a.pass().unwrap(), e.db().last_seq());

        let latest = tmp("rt-latest");
        let (seq, term) = restore(&*store, &latest, Until::Latest, &tmp("rt-s1")).unwrap();
        assert_eq!((seq, term), (e.db().last_seq(), 1));
        assert_eq!(rows(&latest), e.db().scan(b"").unwrap());

        let at_mid = tmp("rt-mid");
        let (seq, _) = restore(&*store, &at_mid, Until::Seq(mid), &tmp("rt-s2")).unwrap();
        assert_eq!(seq, mid);
        assert_eq!(rows(&at_mid), mid_rows);
    }

    /// Point in time: a restore until a wall-clock instant gets exactly
    /// what was archived by then.
    #[test]
    fn restore_until_a_time_stops_at_what_was_archived_by_then() {
        let e = engine("ms");
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("ms-store")));
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let mut a = Archiver::start(
            store.clone(),
            source,
            Arc::new(Acked::default()),
            1,
            tmp("ms-scratch"),
        )
        .unwrap();
        put(&e, 0, 40);
        a.pass().unwrap();
        let (then, then_rows) = (e.db().last_seq(), e.db().scan(b"").unwrap());
        std::thread::sleep(Duration::from_millis(20));
        let boundary = now_ms();
        std::thread::sleep(Duration::from_millis(20));
        put(&e, 40, 80);
        a.pass().unwrap();
        let out = tmp("ms-out");
        let (seq, _) = restore(&*store, &out, Until::Ms(boundary), &tmp("ms-s")).unwrap();
        assert_eq!(seq, then);
        assert_eq!(rows(&out), then_rows);
    }

    /// Timelines never mix: a new term archives into its own directory
    /// from a fresh snapshot; restore picks the newest term that covers
    /// the target, an older one for a target before the newer began.
    #[test]
    fn each_term_archives_its_own_timeline() {
        let e = engine("tl");
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("tl-store")));
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let mut t1 = Archiver::start(
            store.clone(),
            source.clone(),
            Arc::new(Acked::default()),
            1,
            tmp("tl-s1"),
        )
        .unwrap();
        put(&e, 0, 30);
        t1.pass().unwrap();
        let (early, early_rows) = (e.db().last_seq(), e.db().scan(b"").unwrap());
        put(&e, 30, 40);
        let mut t2 = Archiver::start(
            store.clone(),
            source,
            Arc::new(Acked::default()),
            2,
            tmp("tl-s2"),
        )
        .unwrap();
        put(&e, 40, 60);
        t2.pass().unwrap();
        let keys = store.list("").unwrap();
        assert!(
            keys.iter().any(|k| k.starts_with("term-0000000001/")),
            "{keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.starts_with("term-0000000002/snap/")),
            "{keys:?}"
        );

        let (seq, term) = restore(&*store, &tmp("tl-latest"), Until::Latest, &tmp("tl-x")).unwrap();
        assert_eq!((seq, term), (e.db().last_seq(), 2));
        let out = tmp("tl-early");
        let (seq, term) = restore(&*store, &out, Until::Seq(early), &tmp("tl-y")).unwrap();
        assert_eq!((seq, term), (early, 1));
        assert_eq!(rows(&out), early_rows);
    }

    /// The archive holds log release: frames it has not uploaded are
    /// never dropped, even with no standby connected.
    #[test]
    fn the_archive_holds_log_release_until_it_uploads() {
        let dir = tmp("hold");
        let e = engine_at(dir.clone());
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("hold-store")));
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let acked = Arc::new(Acked::default());
        let mut a = Archiver::start(store.clone(), source, acked, 1, tmp("hold-s")).unwrap();
        let base = e.db().last_seq();
        put(&e, 0, 200); // several flushes → several archive files
        assert!(
            e.db().log_since(base, usize::MAX).is_ok(),
            "frames released before upload"
        );
        a.pass().unwrap();
        let files = std::fs::read_dir(dir.join("rlog"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert!(
            files <= 1,
            "uploaded archive files were not released ({files} left)"
        );
    }

    /// Just enough S3 for a key space: PUT, GET, and ListObjectsV2
    /// (list-type=2, prefix) — no pagination.
    fn fake_s3() -> (
        String,
        Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let keys: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>> = Arc::default();
        let k = keys.clone();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, target) = (
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                );
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let key = path.trim_start_matches("/bkt/").to_string();
                let mut map = k.lock().unwrap();
                let (status, out) = match method.as_str() {
                    "PUT" => {
                        map.insert(key, body);
                        ("200 OK", Vec::new())
                    }
                    "GET" if query.contains("list-type=2") => {
                        let prefix = query
                            .split('&')
                            .find_map(|p| p.strip_prefix("prefix="))
                            .unwrap_or("")
                            .replace("%2F", "/");
                        let mut ks: Vec<&String> =
                            map.keys().filter(|k| k.starts_with(&prefix)).collect();
                        ks.sort();
                        let items: String = ks
                            .iter()
                            .map(|k| format!("<Contents><Key>{k}</Key></Contents>"))
                            .collect();
                        ("200 OK", format!("<ListBucketResult><IsTruncated>false</IsTruncated>{items}</ListBucketResult>").into_bytes())
                    }
                    _ => match map.get(&key) {
                        Some(b) => ("200 OK", b.clone()),
                        None => (
                            "404 Not Found",
                            b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                        ),
                    },
                };
                drop(map);
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(&out);
            }
        });
        (endpoint, keys)
    }
}
