package com.aikoql.client;

// The SDK's one classified exception (the repo's classified-error idiom):
// every protocol error keeps the frozen SDK-012 fields
// (code/retryable/suggestion/request_id) — a mapped error never loses its
// code. Mirrors crates/sdk/rust/src/error.rs.

/** A structured error from the MCP server (MRFC-0040 error codes) or the
 * wire layer. Unchecked: SDK call sites throw, they never wrap. */
public class AikoqlException extends RuntimeException {
    private final String code;
    private final boolean retryable;
    private final String suggestion;
    private final String requestId;

    public AikoqlException(String code, String message, boolean retryable,
                           String suggestion, String requestId) {
        super("[" + code + "] " + message);
        this.code = code;
        this.retryable = retryable;
        this.suggestion = suggestion;
        this.requestId = requestId;
    }

    public AikoqlException(String code, String message, boolean retryable, String suggestion) {
        this(code, message, retryable, suggestion, null);
    }

    public AikoqlException(String code, String message) {
        this(code, message, false, "");
    }

    public String getCode() {
        return code;
    }

    public boolean isRetryable() {
        return retryable;
    }

    public String getSuggestion() {
        return suggestion;
    }

    public String getRequestId() {
        return requestId;
    }

    /** The frozen TIMEOUT (retryable): a deadline elapsed before a response. */
    static AikoqlException deadline() {
        return new AikoqlException(
                "TIMEOUT",
                "no response within the deadline",
                true,
                "Retry with backoff; the request may have committed.");
    }

    /** A call on a closed client fails observably (§7 principle 11): a dead
     * connection never deadlocks the caller, and a fresh dial recovers it. */
    static AikoqlException unavailable() {
        return new AikoqlException("UNAVAILABLE", "the client is closed", false, "Connect again.");
    }

    static AikoqlException protocolError(long requestId, long responseId) {
        return new AikoqlException(
                "PROTOCOL_ERROR",
                "response id " + responseId + " does not match request " + requestId,
                false,
                "Check SDK/server version pairing.");
    }

    static AikoqlException versionMismatch(String server) {
        return new AikoqlException(
                "VERSION_MISMATCH",
                "server version " + server + " is older than the SDK minimum "
                        + AikoqlClient.MIN_SERVER_VERSION,
                false,
                "Upgrade the aikoql-mcp server to a supported version");
    }

    static AikoqlException invalidArgument(String message) {
        return new AikoqlException("INVALID_ARGUMENT", message);
    }

    static AikoqlException io(String message) {
        return new AikoqlException("IO", message, false, "Verify the server is reachable.");
    }

    static AikoqlException json(String message) {
        return new AikoqlException("JSON", message, false, "The frame was not valid JSON.");
    }

    /** §19: an over-cap frame tripped the 1 MiB cap mid-accumulation — the
     * stream is desynced, the transport is poisoned. */
    static AikoqlException frameTooLarge() {
        return new AikoqlException(
                "FRAME_TOO_LARGE",
                "response frame exceeds the 1 MiB cap",
                false,
                "The server sent an over-cap frame; reconnect.");
    }
}
