//! AG-61 — a stored memory is semantically searchable at once. RED: the
//! memory's embedding was attached asynchronously by the semantic engine,
//! and memory_search's freshness wait covers the index, not the embedding —
//! the vector leg drops an unembedded object, so a memory searched right
//! after it was stored (an agent's store-then-recall in one turn) was
//! invisible. LongMemEval surfaced it: a second subject's fresh haystack
//! returned 0 hits. Needs the local embedding model; skips without one.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn models_root() -> Option<String> {
    let root = match std::env::var("AIKOQL_TEST_MODEL_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => std::path::PathBuf::from(
            std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?,
        )
        .join(".aikoql")
        .join("models"),
    };
    root.join(aikoql_semantic::provider::model_slug(
        aikoql_semantic::provider::DEFAULT_MODEL_ID,
    ))
    .is_dir()
    .then(|| root.to_string_lossy().into_owned())
}

#[test]
fn ms001_a_memory_is_semantically_searchable_right_after_store() {
    let Some(models) = models_root() else {
        eprintln!("[SKIP] no local embedding model (run `aikoql model install`)");
        return;
    };
    let root = std::env::temp_dir().join(format!("memsem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .args([
            "serve",
            root.join("db").to_str().unwrap(),
            "--model-dir",
            &models,
        ])
        .args(["--memory-dir", root.join("memory").to_str().unwrap()])
        .env("HOME", root.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut id = 0;
    let mut call = |method: &str, params: J| -> J {
        id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{req}").unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            if let Ok(f) = serde_json::from_str::<J>(line.trim()) {
                if f["id"] == id {
                    return f;
                }
            }
        }
    };
    call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    let mut tool = |name: &str, args: J| -> J {
        let f = call("tools/call", json!({"name": name, "arguments": args}));
        let text = f["result"]["content"][0]["text"].as_str().unwrap_or("{}");
        serde_json::from_str(text).unwrap_or(J::Null)
    };
    let mut koids = Vec::new();
    for (name, body) in [
        (
            "commute",
            "my daily commute takes forty-five minutes on the train",
        ),
        ("surf", "I went surfing in Bali last spring"),
        ("food", "street food in Bangkok is my favourite"),
    ] {
        let r = tool(
            "memory_store",
            json!({"name": name, "description": body, "content": body}),
        );
        koids.push(r["koid"].clone());
    }
    // the structural pin: the object memory_store returns already carries its
    // embedding (no wait for the asynchronous engine)
    let ko = tool("get", json!({"koid": koids[0]}));
    assert!(
        ko["semantic"].is_object(),
        "the stored memory is embedded: {ko}"
    );
    // no freshness wait: the memory must already carry its embedding
    let r = tool(
        "memory_search",
        json!({"query": "how long does my trip to the office take", "max_results": 3,
               "wait_for_freshness_ms": 0}),
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
    // AG-33: no trust declared → undeclared → the name comes back fenced
    assert!(
        r["results"][0]["name"]
            .as_str()
            .is_some_and(|n| n.contains("commute")),
        "{r}"
    );
    assert_eq!(r["results"][0]["trust"], "undeclared", "{r}");
}
