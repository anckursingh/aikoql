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
