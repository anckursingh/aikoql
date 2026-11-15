package com.aikoql.client;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

// The §3.6 prepared statement: prepare/bind/execute/close. The query
// compiles client-side on every execute — the abstraction precedes a native
// prepare protocol, so the statement holds no server-side plan (nothing to
// invalidate, nothing lost across a server restart). Bind validates the
// binding and sends the named parameters, which the server binds.
// Mirrors crates/sdk/go/prepared.go.
public final class PreparedStatement {
    private static final Pattern PLACEHOLDER = Pattern.compile(":([a-zA-Z_][a-zA-Z0-9_]*)");

    private final Connection conn;
    private final String query;
    private final List<String> params;
    private boolean closed;
    private Map<String, Object> bound;

    PreparedStatement(Connection conn, String query) {
        this.conn = conn;
        this.query = query;
        if (query.trim().isEmpty()) {
            throw AikoqlException.invalidArgument("prepared statement query is empty");
        }
        this.params = new ArrayList<>();
        Matcher m = PLACEHOLDER.matcher(query);
        while (m.find()) {
            if (!params.contains(m.group(1))) params.add(m.group(1));
        }
    }

    /** Binds the placeholders; the statement stays reusable. */
    public PreparedStatement bind(Map<String, Object> params) {
        this.bound = params;
        return this;
    }

    /** Bind-and-execute in one call. */
    public Json.Value execute(Map<String, Object> params, Deadline dl) throws AikoqlException {
        return bind(params).execute(dl);
    }

    /** Validates the binding and runs the query; the server binds the
     * named parameters. */
    public Json.Value execute(Deadline dl) throws AikoqlException {
        if (closed) throw AikoqlException.invalidArgument("prepared statement is closed");
        Map<String, Object> b = bound == null ? Map.of() : bound;
        check(params, b);
        return conn.aikoql(query, "", b, dl);
    }

    /** Closes the statement; further executes are refused. There is no
     * server-side handle yet, so this only marks the local state. */
    public void close() {
        closed = true;
    }

    /** check validates the binding: exact placeholder set, scalar values
     * only. */
    static void check(List<String> params, Map<String, Object> bound) {
        if (bound.size() != params.size()) {
            throw AikoqlException.invalidArgument(
                    "bound " + bound.size() + " parameters, the statement has "
                            + params.size() + " (" + String.join(", ", params) + ")");
        }
        for (String name : params) {
            if (!bound.containsKey(name)) {
                throw AikoqlException.invalidArgument("parameter :" + name + " is not bound");
            }
            Object value = bound.get(name);
            if (!(value == null || value instanceof String || value instanceof Number
                    || value instanceof Boolean)) {
                throw AikoqlException.invalidArgument(
                        "parameter :" + name + " must be a scalar, got "
                                + value.getClass().getSimpleName());
            }
        }
    }
}
