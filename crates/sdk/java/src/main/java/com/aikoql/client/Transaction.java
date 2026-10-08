package com.aikoql.client;

import java.util.LinkedHashMap;
import java.util.Map;

// The §3.5 transaction handle: the txn_id is a first-class field — it
// never leaks as a bare tool argument a caller threads between call sites.
// A commit that errors leaves the handle open (the server dedupes by
// txn_id, so the caller may retry); a closed handle refuses further use
// with INVALID_ARGUMENT. Mirrors crates/sdk/typescript/src/tx.ts.
public final class Transaction {
    private final Connection conn;
    private final String txnId;
    private boolean closed;

    Transaction(Connection conn, String txnId) {
        this.conn = conn;
        this.txnId = txnId;
    }

    /** The txn_id (for retries — commit dedupes by it, P5-M20). */
    public String id() {
        return txnId;
    }

    /** True once commit or rollback closed the handle. */
    public boolean done() {
        return closed;
    }

    /** Stages one write. */
    public void execute(String action, String typeName, Map<String, Object> properties,
                        Deadline dl) throws AikoqlException {
        Map<String, Object> op = new LinkedHashMap<>();
        op.put("action", action);
        if (!typeName.isEmpty()) op.put("type_name", typeName);
        if (properties != null) op.put("properties", properties);
        step("txn_stage", op, dl);
    }

    /** Applies the staged writes and closes the handle. */
    public Json.Value commit(Deadline dl) throws AikoqlException {
        return step("txn_commit", null, dl);
    }

    /** Discards the staged writes and closes the handle. */
    public void rollback(Deadline dl) throws AikoqlException {
        step("txn_rollback", null, dl);
    }

    private Json.Value step(String name, Map<String, Object> op, Deadline dl)
            throws AikoqlException {
        if (closed) throw AikoqlException.invalidArgument("transaction " + txnId + " is closed");
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("txn_id", txnId);
        if (op != null) args.put("op", op);
        Json.Value raw = conn.callTool(name, args, dl);
        if (name.equals("txn_commit") || name.equals("txn_rollback")) closed = true;
        return raw;
    }
}
