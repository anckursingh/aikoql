//! Extracted verbatim from server.rs (PRR-7). No behavior changes.

use crate::{json, Write, J};
/// JSON-RPC call result: `Ok(value)` is a result payload, `Err((code,
/// message))` is a JSON-RPC error. (Moved here from server.rs, PRR-7.)
pub(crate) type ToolResult = Result<J, (i64, String)>;

pub(crate) fn write_frame(out: &mut impl Write, frame: J) {
    if writeln!(out, "{}", frame).is_err() || out.flush().is_err() {
        // Connection died — the caller's next read ends the session.
    }
}
pub(crate) fn err_frame(id: &J, code: i64, message: &str) -> J {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P5-M17 (MCP write path): a large tool result must not fragment into
    /// per-token writes. `write_frame` over a raw `TcpStream` serialized
    /// through `Display`, and each JSON token became its own send() syscall —
    /// a 127 KB frame paid ~20k syscalls and took ~700-900 ms on Windows
    /// (measured: mcp_mode structured_filter 725 ms @N=1000 vs 9.11 ms
    /// embedded). RED: pin the bounded-write contract, then serialize once
    /// and write_all.
    struct CountingWriter {
        calls: usize,
        bytes: Vec<u8>,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_frame_bounds_write_calls_for_large_results() {
        // 500 rows × ~6 properties ≈ the structured_filter response shape.
        let rows: Vec<J> = (0..500)
            .map(|i| {
                json!({
                    "koid": format!("{:064x}", i),
                    "type_name": "note",
                    "version": 1,
                    "properties": {"topic": "pet", "body": "cats-and-dogs", "seq": i},
                    "extensions": {"scope": "session"}
                })
            })
            .collect();
        let frame = json!({"results": rows});
        let mut w = CountingWriter {
            calls: 0,
            bytes: Vec::new(),
        };
        write_frame(&mut w, frame.clone());
        assert!(
            w.calls <= 2,
            "a large frame must be one write_all (+flush), got {} write calls",
            w.calls
        );
        // The frame on the wire is still the full serialized JSON line.
        let want = format!("{}\n", serde_json::to_string(&frame).unwrap());
        assert_eq!(String::from_utf8(w.bytes).unwrap(), want);
    }
}
