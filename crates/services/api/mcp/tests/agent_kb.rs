//! A knowledge base that `ingest-dir` builds must be usable by the coding agent
//! that asks for it, and cheap to ask: the agent's MCP session reads what the
//! CLI stored, and `compile_context` can return just the markdown.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_aikoql");

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aikoql-agentkb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::create_dir_all(d.join("models")).unwrap(); // empty: no embedding model, ingestion still works
    std::fs::write(
        d.join("src/lib.rs"),
        "/// Decide whether a project is idle.\npub fn decide(idle_secs: u64) -> bool { idle_secs > 3 * 86_400 }\n\
         /// Stop a project.\npub fn stop() {}\n",
    )
    .unwrap();
    d
}

/// `aikoql ingest-dir`, returning the snapshot KOID it prints.
fn ingest(dir: &Path) -> (String, PathBuf) {
    let db = dir.join("kb");
    let out = Command::new(BIN)
        .args(["ingest-dir"])
        .arg(dir.join("src"))
        .arg(&db)
        .arg("--model-dir")
        .arg(dir.join("models"))
        .output()
        .expect("run ingest-dir");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "ingest-dir failed: {text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let koid = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("KOID: "))
        .unwrap_or_else(|| panic!("no KOID in: {text}"))
        .to_string();
    (koid, db)
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(db: &Path, models: &Path) -> Mcp {
        let mut child = Command::new(BIN)
            .arg("serve")
            .arg(db)
            .arg("--model-dir")
            .arg(models)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("serve");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            out,
            id: 0,
        };
        m.rpc(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        );
        m
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "server closed");
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            if v.get("id") == Some(&json!(self.id)) {
                return v;
            }
        }
    }
    /// A tool call as an ordinary agent: no subject, no roles, nothing special.
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{r}"));
        let v: Value = serde_json::from_str(text).unwrap();
        v.get("data").cloned().unwrap_or(v)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn what_ingest_dir_stores_is_readable_by_the_agents_session() {
    let d = scratch("read");
    let (koid, db) = ingest(&d);
    let mut m = Mcp::start(&db, &d.join("models"));
    let r = m.tool(
        "compile_context",
        json!({"koid": koid, "task": "decide idle project", "token_budget": 800}),
    );
    assert!(
        r.get("error").is_none(),
        "the agent must be able to read what ingest-dir stored: {r}"
    );
    let md = r["context_markdown"]
        .as_str()
        .unwrap_or_else(|| panic!("{r}"));
    assert!(md.contains("decide"), "the answer names the function: {md}");
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn compile_context_can_return_only_the_markdown() {
    let d = scratch("pkg");
    let (koid, db) = ingest(&d);
    let mut m = Mcp::start(&db, &d.join("models"));
    let args = json!({"koid": koid, "task": "decide idle project", "token_budget": 800});
    let full = m.tool("compile_context", args.clone());
    assert!(
        full.get("package").is_some(),
        "the default keeps the structured package: {full}"
    );
    let mut lean_args = args;
    lean_args["include_package"] = json!(false);
    let lean = m.tool("compile_context", lean_args);
    assert!(
        lean.get("package").is_none(),
        "include_package=false leaves it out: {lean}"
    );
    assert_eq!(
        lean["context_markdown"], full["context_markdown"],
        "the useful part is unchanged"
    );
    assert!(
        lean.to_string().len() < full.to_string().len() / 2,
        "and the answer is much smaller"
    );
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_ingest_hint_names_a_command_that_exists() {
    let d = scratch("hint");
    let out = Command::new(BIN)
        .args(["ingest-dir"])
        .arg(d.join("src"))
        .arg(d.join("kb"))
        .arg("--model-dir")
        .arg(d.join("models"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !text.contains("aikoql shell"),
        "the shell does not take compile_context: {text}"
    );
    assert!(
        text.contains("MCP") && text.contains("compile_context"),
        "the hint points at the MCP tool: {text}"
    );
    let _ = std::fs::remove_dir_all(d);
}

// ---- The guide and the skill say only things that are true of this build ----

fn repo(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn the_skill_is_part_b_of_the_guide() {
    let guide = repo("docs/AGENT-SETUP.md");
    let skill = repo("skills/aikoql/SKILL.md");
    let b = &guide[guide.find("## Part B: use it").expect("Part B")
        ..guide.find("## Troubleshooting").expect("Troubleshooting")];
    assert!(
        skill.contains(b.trim_end()),
        "skills/aikoql/SKILL.md must carry Part B of docs/AGENT-SETUP.md verbatim"
    );
    assert!(
        skill.starts_with("---\nname: aikoql\ndescription: "),
        "a skill needs its frontmatter"
    );
}

#[test]
fn every_tool_and_flag_the_guide_names_exists() {
    let guide = repo("docs/AGENT-SETUP.md");
    let registry = repo("crates/services/api/mcp/src/tool_registry.rs");
    for tool in ["compile_context", "memory_store", "memory_search", "health"] {
        assert!(guide.contains(tool), "the guide should mention {tool}");
        assert!(
            registry.contains(&format!("\"name\": \"{tool}\"")),
            "the guide names a tool that does not exist: {tool}"
        );
    }
    assert!(
        registry.contains("include_package"),
        "the guide tells agents to pass include_package"
    );
    let cli = repo("crates/services/api/mcp/src/cli.rs");
    for flag in ["ingest-dir", "--incremental"] {
        assert!(
            guide.contains(flag) && cli.contains(flag),
            "{flag} must be both documented and real"
        );
    }
}
