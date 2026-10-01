// The SDK error surface: one classified error type (the repo's
// classified-error idiom). Every protocol error keeps the frozen SDK-012
// fields (code/message/retryable/suggestion) — a mapped error never loses
// its code. The server's error taxonomy (MRFC-0040 / SDK-012) is frozen;
// this SDK only constructs the codes it meets on the wire plus its own
// transport folds (IO/JSON).

import { MIN_SERVER_VERSION } from "./client.ts";

/**
 * A structured error from the MCP server: the tool-level ok/error envelope
 * and RPC-level failures both carry it. Transport failures fold into the
 * nearest code ("IO" for a dead connection, "JSON" for a malformed frame)
 * so callers always match on `code`.
 */
export class McpError extends Error {
  code: string;
  retryable: boolean;
  suggestion: string;

  constructor(code: string, message: string, retryable = false, suggestion = "") {
    super(`[${code}] ${message}`);
    this.name = "McpError";
    this.code = code;
    this.retryable = retryable;
    this.suggestion = suggestion;
  }

  /** The frozen TIMEOUT (retryable): a deadline elapsed before a response. */
  static deadline(): McpError {
    return new McpError(
      "TIMEOUT",
      "no response within the deadline",
      true,
      "Retry with backoff; the request may have committed.",
    );
  }

  /** A call on a closed client fails observably (§7 principle 11): a dead
   * connection never deadlocks the caller, and a fresh dial recovers it. */
  static unavailable(): McpError {
    return new McpError(
      "UNAVAILABLE",
      "the client is closed",
      false,
      "Connect again.",
    );
  }

  static protocolError(requestId: number, responseId: number): McpError {
    return new McpError(
      "PROTOCOL_ERROR",
      `response id ${responseId} does not match request ${requestId}`,
      false,
      "Check SDK/server version pairing.",
    );
  }

  static versionMismatch(server: string): McpError {
    return new McpError(
      "VERSION_MISMATCH",
      `server version ${JSON.stringify(server)} is older than the SDK minimum ${MIN_SERVER_VERSION}`,
      false,
      "Upgrade the aikoql-mcp server to a supported version",
    );
  }

  static invalidArgument(message: string): McpError {
    return new McpError("INVALID_ARGUMENT", message, false, "");
  }

  /** A transport failure, folded to the "IO" code. */
  static io(message: string): McpError {
    return new McpError("IO", message, false, "");
  }

  /** A malformed frame or payload, folded to the "JSON" code. */
  static json(message: string): McpError {
    return new McpError("JSON", message, false, "");
  }
}
