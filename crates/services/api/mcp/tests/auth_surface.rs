//! P3-M1 (auth004): startup fail-closed pins that need the real binary —
//! config layers, env arming, and the exit(2) path before the kernel opens.
//! Unit-level guards live in src/tests.rs (auth004_remote_http_refused_without_auth).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aikoql-mcp"))
}

fn tmp_db(tag: &str) -> String {
    std::env::temp_dir()
        .join(format!("mnemo-{tag}-{}.redb", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

/// Spawn `serve`, expect exit code 2 within 5s; kill + panic if the server
/// stays up (fail-closed is the point — an armed-but-unauth remote HTTP
/// surface would just keep serving).
fn expect_exit2(args: &[&str], tag: &str, envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(bin());
    cmd.args(["serve"])
        .args(args)
        .arg(tmp_db(tag))
        .env_remove("AIKOQL_ALLOW_REMOTE_HTTP")
        .env_remove("AIKOQL_ADMIN_PASSWORD")
        .env_remove("AIKOQL_TCP_TOKEN")
        .envs(envs.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child: Child = cmd.spawn().expect("spawn aikoql-mcp");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(code) => {
                assert_eq!(code.code(), Some(2), "expected exit 2");
                let mut err = String::new();
                use std::io::Read;
                child
                    .stderr
                    .take()
                    .expect("stderr")
                    .read_to_string(&mut err)
                    .expect("read stderr");
                return err;
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{tag}: server still serving — fail-closed startup missing");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[test]
fn remote_http_without_credentials_refuses_to_serve() {
    // Armed for remote HTTP, no [auth] and no bootstrap password — the
    // credentials gate must refuse before anything listens.
    let err = expect_exit2(
        &["--metrics-addr", "0.0.0.0:9091"],
        "remote-no-auth",
        &[("AIKOQL_ALLOW_REMOTE_HTTP", "1")],
    );
    assert!(
        err.contains("allow_remote_http"),
        "stderr must name the refusal: {err}"
    );
}

#[test]
fn metrics_nonloopback_refused_by_default() {
    let err = expect_exit2(
        &["--metrics-addr", "0.0.0.0:9091"],
        "metrics-nonloopback",
        &[],
    );
    assert!(
        err.contains("metrics-addr"),
        "stderr must name the listener: {err}"
    );
}
