package com.aikoql.client;

import java.security.SecureRandom;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.locks.ReentrantLock;

// One MCP JSON-RPC session to an aikoql-mcp server: newline frames, id
// correlation, the tools/call envelope — the frozen §3.3 semantics (a
// smaller id is skipped, a larger id is PROTOCOL_ERROR, deadline/aborted
// reads map to the retryable TIMEOUT, a call on a closed session is
// UNAVAILABLE before the transport is touched). One in-flight call per
// session: calls serialize on a lock, and a stream holds it for its whole
// life. Mirrors crates/sdk/typescript/src/client.ts (the Go and Rust wire
// layers are the same contract). The transport is private to the session:
// the D-15 native protocol replacement touches only Transport.
public final class Connection implements AutoCloseable {
    private static final SecureRandom RNG = new SecureRandom();

    /** The frozen §3.3 correlation verdicts, restated (the Go corr*
     * constants): smaller ids skip, larger ids are PROTOCOL_ERROR, equal
     * ids match. */
    enum Corr { SKIP, PROTOCOL, MATCH }

    /** The frozen §3.3 correlation rules: a smaller id is skipped (id-less,
     * notification, duplicate, or late — never an error), a larger id is
     * PROTOCOL_ERROR, equal ids match. */
    static Corr classifyID(long want, long got) {
        if (got < want) return Corr.SKIP;
        if (got > want) return Corr.PROTOCOL;
        return Corr.MATCH;
    }

    final Transport transport;
    final ReentrantLock lock = new ReentrantLock();
    private int nextId;
    private boolean closed;
    private String token;
    private String name = "aikoql-java";
    private String version = AikoqlClient.VERSION;

    Connection(String addr) {
        this.transport = Transport.connect(addr, AikoqlClient.DIAL_TIMEOUT_MS);
    }

    /** Sends the --tcp-token credential in the initialize handshake
     * (required by every TCP server since P3-M1). */
    public Connection withToken(String token) {
        this.token = token;
        return this;
    }

    /** Sets the MCP client identity advertised at initialize. */
    public Connection withClientInfo(String name, String version) {
        this.name = name;
        this.version = version;
        return this;
    }

    /** Closes the session. Safe to call more than once. */
    @Override
    public void close() {
        lock.lock();
        try {
            closed = true;
            transport.close();
        } finally {
            lock.unlock();
        }
    }

    /** Sends one JSON-RPC request and returns its result frame, skipping
     * pushed notifications by id correlation (§3.3). The lock is held by
     * the caller. */
    private Json.Value request(String method, Map<String, Object> params, Deadline dl)
            throws AikoqlException {
        if (closed) throw AikoqlException.unavailable();
        long id = ++nextId;
        Map<String, Object> frame = new LinkedHashMap<>();
        frame.put("jsonrpc", "2.0");
        frame.put("id", id);
        frame.put("method", method);
        if (params != null) frame.put("params", params);
        try {
            transport.write(Json.stringify(Json.from(frame)));
        } catch (AikoqlException e) {
            closed = true; // the transport is gone — latch
            throw e;
        }
        try {
            for (;;) {
                String line = transport.readLine(dl);
                if (line == null) {
                    // The server closed (or half-closed): latch — later
                    // calls fail fast with UNAVAILABLE instead of dialing
                    // a dead socket.
                    closed = true;
                    throw AikoqlException.io("connection closed by the server");
                }
                Json.Value resp;
                try {
                    resp = Json.parse(line);
                } catch (AikoqlException e) {
                    continue; // tolerate non-JSON noise frames
                }
                Json.Value ridV = Json.dotGet(resp, "id");
                if (!(ridV instanceof Json.Num n)) continue; // not our numeric correlation
                long rid = (long) n.v();
                switch (classifyID(id, rid)) {
                    // id-less, notification, duplicate, or late — never an error
                    case SKIP -> { continue; }
                    case PROTOCOL -> throw AikoqlException.protocolError(id, rid);
                    case MATCH -> { }
                }
                Json.Value err = Json.dotGet(resp, "error");
                if (err != null) throw rpcError(err);
                Json.Value result = Json.dotGet(resp, "result");
                return result == null ? Json.Null.NULL : result;
            }
        } catch (AikoqlException e) {
            if ("FRAME_TOO_LARGE".equals(e.getCode())) {
                closed = true; // the stream is desynced — latch
            }
            throw e;
        }
    }

    /** A dead transport poisons the session: every later call fails fast
     * with UNAVAILABLE before the wire is touched (§19). Package-private
     * for ResultSet, which owns the stream read path. */
    void latchClosed() {
        closed = true;
    }

    /** An RPC-level error keeps only code/message; codes normalize to
     * their string form (string-encoded and numeric codes both occur). */
    static AikoqlException rpcError(Json.Value e) {
        String code = "";
        String message = "";
        if (e instanceof Json.Obj o) {
            code = Json.jsonStr(o.fields.get("code"));
            message = Json.jsonStr(o.fields.get("message"));
        }
        return new AikoqlException(code.isEmpty() ? "INTERNAL" : code, message);
    }

    /** Mirrors the Go SDK's dotted-int tuple: non-numeric segments become
     * -1 (never >=). */
    static int[] parseVersion(String v) {
        String[] parts = v.split("\\.");
        int[] out = new int[parts.length];
        for (int i = 0; i < parts.length; i++) {
            try {
                out[i] = Integer.parseInt(parts[i]);
            } catch (NumberFormatException e) {
                out[i] = -1;
            }
        }
        return out;
    }

    /** Compares two dotted version tuples segment by segment. */
    static boolean versionLess(int[] a, int[] b) {
        for (int i = 0; i < Math.min(a.length, b.length); i++) {
            if (a[i] != b[i]) return a[i] < b[i];
        }
        return a.length < b.length;
    }

    /** Performs the MCP handshake (protocol version, client info, token)
     * and enforces the ND-12 version contract: a server older than
     * MIN_SERVER_VERSION fails fast with VERSION_MISMATCH. */
    public void initialize(Deadline dl) throws AikoqlException {
        Map<String, Object> params = new LinkedHashMap<>();
        params.put("protocolVersion", "2024-11-05");
        params.put("capabilities", new LinkedHashMap<String, Object>());
        Map<String, Object> ci = new LinkedHashMap<>();
        ci.put("name", name);
        ci.put("version", version);
        params.put("clientInfo", ci);
        if (token != null) params.put("token", token);
        Json.Value raw;
        lock.lock();
        try {
            raw = request("initialize", params, dl);
        } finally {
            lock.unlock();
        }
        String server = Json.jsonStr(Json.dotGet(raw, "serverInfo.version"));
        if (versionLess(parseVersion(server), parseVersion(AikoqlClient.MIN_SERVER_VERSION))) {
            throw AikoqlException.versionMismatch(server);
        }
    }

    /** Establishes session identity (MRFC-0040); subsequent calls inherit
     * it. On TCP the identity is server-assigned by --tcp-token, so
     * agent_id must be omitted there — only run_id is per-session. */
    public void sessionInit(String agentId, String runId, String tenant, List<String> roles,
                            Deadline dl) throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        if (agentId != null) args.put("agent_id", agentId);
        if (runId != null) args.put("run_id", runId);
        if (tenant != null) args.put("tenant", tenant);
        if (roles != null) args.put("roles", roles);
        lock.lock();
        try {
            request("session/init", args, dl);
        } finally {
            lock.unlock();
        }
    }

    /** Calls any registered MCP tool by name and returns its data payload —
     * the escape hatch for tools without a typed wrapper here. */
    public Json.Value callTool(String name, Map<String, Object> args, Deadline dl)
            throws AikoqlException {
        Map<String, Object> params = new LinkedHashMap<>();
        params.put("name", name);
        if (args != null) params.put("arguments", args);
        Json.Value raw;
        lock.lock();
        try {
            raw = request("tools/call", params, dl);
        } finally {
            lock.unlock();
        }
        return decodeToolEnvelope(name, raw);
    }

    /** The pure tools/call envelope decode: the inner text payload out of
     * the result frame. Mirrors the Python SDK: an absent "ok" means
     * success; the "data" field, when present, wraps the payload; ok:false
     * throws the mapped error. */
    static Json.Value decodeToolEnvelope(String toolName, Json.Value raw)
            throws AikoqlException {
        String text = Json.jsonStr(Json.dotGet(raw, "content.0.text"));
        Json.Value payload;
        try {
            payload = Json.parse(text);
        } catch (AikoqlException e) {
            throw AikoqlException.json(e.getMessage());
        }
        Json.Value ok = Json.dotGet(payload, "ok");
        if (ok instanceof Json.Bool b && !b.v()) {
            Json.Value err = Json.dotGet(payload, "error");
            if (err instanceof Json.Obj o) {
                String code = Json.jsonStr(o.fields.get("code"));
                if (code.isEmpty()) code = "INTERNAL";
                String message = Json.jsonStr(o.fields.get("message"));
                boolean retryable =
                        o.fields.get("retryable") instanceof Json.Bool rb && rb.v();
                String suggestion = Json.jsonStr(o.fields.get("suggestion"));
                throw new AikoqlException(code, message, retryable, suggestion);
            }
            throw new AikoqlException(
                    "INTERNAL", "tool " + toolName + " failed without an error envelope");
        }
        Json.Value data = Json.dotGet(payload, "data");
        if (data != null) return data;
        return payload;
    }

    /** Runs a streaming query. Yields the response frame (the first data
     * chunk — for total_chunks == 1 there is no notify at all), then each
     * notify chunk until its done flag. Closing the set (or an abort
     * mid-read) cancels the read and releases the session — the session
     * must not be shared while the stream is open (the Go SDK's caveat,
     * inherited). */
    public ResultSet queryStream(String query, String subject, Deadline dl)
            throws AikoqlException {
        Map<String, Object> params = new LinkedHashMap<>();
        params.put("query", query);
        if (!subject.isEmpty()) params.put("subject", subject);
        lock.lock();
        try {
            if (closed) throw AikoqlException.unavailable();
            long id = ++nextId;
            Map<String, Object> frame = new LinkedHashMap<>();
            frame.put("jsonrpc", "2.0");
            frame.put("id", id);
            frame.put("method", "aikoql/stream");
            frame.put("params", params);
            try {
                transport.write(Json.stringify(Json.from(frame)));
            } catch (AikoqlException e) {
                closed = true; // the transport is gone — latch
                throw e;
            }
            return new ResultSet(this, id, dl);
        } catch (RuntimeException e) {
            lock.unlock();
            throw e;
        }
    }

    // — Typed wrappers — the canonical Database API surface over the MCP
    // tools (crates/sdk/go/tools.go is the schema source).

    /** Commits a knowledge object (or a new version of one). */
    public Json.Value remember(String typeName, String koid, Map<String, Object> properties,
                               Deadline dl) throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("type_name", typeName);
        if (koid != null) args.put("koid", koid);
        if (properties != null) args.put("properties", properties);
        return callTool("remember", args, dl);
    }

    /** Fetches a knowledge object by KOID. */
    public Json.Value get(String koid, String subject, Deadline dl) throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("koid", koid);
        if (!subject.isEmpty()) args.put("subject", subject);
        return callTool("get", args, dl);
    }

    /** Tombstones ("tombstone") or legally erases ("erase") a knowledge
     * object, audit-preserving. */
    public Json.Value forget(String koid, String mode, String subject, Deadline dl)
            throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("koid", koid);
        args.put("mode", mode);
        if (!subject.isEmpty()) args.put("subject", subject);
        return callTool("forget", args, dl);
    }

    /** Runs hybrid recall and returns the scored hits. */
    public Json.Value findSimilar(String text, Long waitForFreshnessMs, Deadline dl)
            throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        if (text != null) args.put("text", text);
        if (waitForFreshnessMs != null) args.put("wait_for_freshness_ms", waitForFreshnessMs);
        Json.Value raw = callTool("find_similar", args, dl);
        Json.Value results = Json.dotGet(raw, "results");
        return results == null ? new Json.Arr() : results;
    }

    /** Runs an AikoQL query and returns the raw result rows. */
    public Json.Value aikoql(String query, String subject, Deadline dl) throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("query", query);
        if (!subject.isEmpty()) args.put("subject", subject);
        return callTool("aikoql", args, dl);
    }

    /** Links two knowledge objects. */
    public Json.Value relate(String from, String to, String relType, String subject, Deadline dl)
            throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("from", from);
        args.put("to", to);
        args.put("rel_type", relType);
        if (!subject.isEmpty()) args.put("subject", subject);
        return callTool("relate", args, dl);
    }

    /** Walks the relationship graph from a KOID. */
    public Json.Value traverse(String koid, String relType, String subject, long depth,
                               Deadline dl) throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("koid", koid);
        args.put("depth", depth);
        if (!relType.isEmpty()) args.put("rel_type", relType);
        if (!subject.isEmpty()) args.put("subject", subject);
        return callTool("traverse", args, dl);
    }

    /** Returns the server health payload. */
    public Json.Value health(Deadline dl) throws AikoqlException {
        return callTool("health", null, dl);
    }

    /** Returns the schema-discovery payload. */
    public Json.Value discoverSchema(Deadline dl) throws AikoqlException {
        return callTool("discover_schema", null, dl);
    }

    /** Returns the server's metrics. */
    public Json.Value metrics(Deadline dl) throws AikoqlException {
        return callTool("metrics", null, dl);
    }

    /** Returns the full lineage of a fact (versions + events). */
    public Json.Value trace(String koid, String subject, Deadline dl) throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("koid", koid);
        if (!subject.isEmpty()) args.put("subject", subject);
        return callTool("trace", args, dl);
    }

    /** Returns the explanation payload for a KO version. */
    public Json.Value explain(String koid, String subject, Long version, Deadline dl)
            throws AikoqlException {
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("koid", koid);
        if (version != null) args.put("version", version);
        if (!subject.isEmpty()) args.put("subject", subject);
        return callTool("explain", args, dl);
    }

    /** Opens a transaction. With no id a random 32-hex id is generated;
     * passing one retries the same begin idempotently (P5-M20). */
    public Transaction begin(String txnId, Deadline dl) throws AikoqlException {
        byte[] raw = new byte[16];
        RNG.nextBytes(raw);
        String id = txnId != null ? txnId : HexFormat.of().formatHex(raw);
        Map<String, Object> args = new LinkedHashMap<>();
        args.put("txn_id", id);
        callTool("txn_begin", args, dl);
        return new Transaction(this, id);
    }

    /** Prepares a query client-side (the §3.6 prepared statement). */
    public PreparedStatement prepare(String query) {
        return new PreparedStatement(this, query);
    }
}
