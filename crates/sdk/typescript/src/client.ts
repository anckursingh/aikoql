// The remote client: MCP JSON-RPC over the Transport. Mirrors the Go
// SDK's wire layer (crates/sdk/go/aikoql.go) and the Rust reference
// (crates/sdk/rust/src/client.rs) — newline frames, id correlation, the
// tools/call envelope — and the frozen §3.3 semantics: a response with a
// smaller id is skipped, a larger id is PROTOCOL_ERROR, deadline/aborted
// reads map to the retryable TIMEOUT, and a call on a closed client is
// UNAVAILABLE before the transport is touched. The transport is private
// to the Client: the D-15 native protocol replacement touches only the
// Transport, not this file (the canonical API does not change when the
// transport changes).

import { randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { McpError } from "./error.ts";
import { TcpTransport, type Transport } from "./tcp.ts";
import type {
  FindSimilarParams,
  KnowledgeObject,
  Metrics,
  RememberParams,
  Remembered,
  ScoredKO,
  SessionParams,
} from "./tools.ts";
import { Tx } from "./tx.ts";

/**
 * The oldest aikoql-mcp server this SDK will talk to (the ND-12 version
 * contract, mirrored from the Go, Python and Rust SDKs).
 */
export const MIN_SERVER_VERSION = "0.2.1";

/** The SDK's own package version, advertised as the client identity:
 * read from package.json so the manifest, not a literal, owns it. */
function packageVersion(): string {
  try {
    const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { version?: string };
    return pkg.version ?? "dev";
  } catch {
    return "dev"; // vendored without the manifest
  }
}
const CLIENT_VERSION = packageVersion();

const DIAL_TIMEOUT_MS = 5000;

interface ClientConfig {
  token?: string;
  name: string;
  version: string;
}

/** Mirrors the Python SDK's int(seg) exactly (the Go Atoi agrees on the
 * canonical cases): only an optionally-signed decimal integer segment is
 * numeric — Number("") would read "" as 0 and "0x10" as 16 where int()
 * refuses (base 10 by default) — everything else becomes -1 (never >=). */
export function parseVersion(v: string): number[] {
  return v.split(".").map((seg) => {
    const s = seg.trim();
    return /^[+-]?\d+$/.test(s) ? Number(s) : -1;
  });
}

/** Compares two dotted version tuples segment by segment. */
export function versionLess(a: number[], b: number[]): boolean {
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    if (a[i] !== b[i]) return a[i]! < b[i]!;
  }
  return a.length < b.length;
}

/** An RPC-level error keeps only code/message; codes normalize to their
 * string form (string-encoded and numeric codes both occur). */
export function rpcError(e: { code?: unknown; message?: string }): McpError {
  const code = e.code === undefined || e.code === null ? "" : String(e.code);
  return new McpError(code === "" ? "INTERNAL" : code, e.message ?? "", false, "");
}

/** The frozen §3.3 correlation rules, restated: a smaller id is skipped
 * (id-less, notification, duplicate, or late — never an error), a larger
 * id is PROTOCOL_ERROR, equal ids match. */
export function classifyID(want: number, got: number): "skip" | "match" | "protocol" {
  if (got < want) return "skip";
  if (got > want) return "protocol";
  return "match";
}

/** The stream loop's notify verdict primitive: the method gate, params
 * extraction and stream-id filter in one place (the aikoqlStream loop and
 * the §16 corpus pin share it). */
export function streamNotify(frame: Record<string, unknown>, streamId: string): { stream_id?: string; done?: boolean } | null {
  if (frame["method"] !== "notifications/notify") return null;
  const p = (frame["params"] ?? null) as {
    stream_id?: string;
    done?: boolean;
  } | null;
  if (p === null || p.stream_id !== streamId) return null;
  return p;
}

/**
 * One MCP JSON-RPC connection to an aikoql-mcp server. One in-flight call
 * per connection (a stream holds the connection for its whole life); calls
 * serialize over a promise chain, the single-threaded stand-in for the
 * Rust client's mutex.
 */
export class Client {
  private transport: Transport;
  private nextId = 0;
  private closed = false;
  private cfg: ClientConfig = { name: "aikoql-ts-sdk", version: CLIENT_VERSION };
  private chain: Promise<unknown> = Promise.resolve();
  // The open stream's release (null when none): close() cancels a
  // mid-stream close through it (§17).
  private streamGate: (() => void) | null = null;

  private constructor(transport: Transport) {
    this.transport = transport;
  }

  /** Opens a TCP connection ("host:port"). The handshake is separate
   * (initialize) so a pooled client can dial first and authenticate later;
   * every aikoql-mcp TCP server requires the token. */
  static async dial(addr: string): Promise<Client> {
    const transport = await TcpTransport.connect(addr, DIAL_TIMEOUT_MS);
    return new Client(transport);
  }

  /** Sends the --tcp-token credential in the initialize handshake
   * (required by every TCP server since P3-M1). */
  withToken(token: string): this {
    this.cfg.token = token;
    return this;
  }

  /** Sets the MCP client identity advertised at initialize. */
  withClientInfo(name: string, version: string): this {
    this.cfg.name = name;
    this.cfg.version = version;
    return this;
  }

  /** Closes the connection. Safe to call more than once, and safe with an
   * open stream: the stream gate is released first — otherwise close (and
   * every call behind it) deadlocks on the chain (§17). */
  async close(): Promise<void> {
    this.closed = true; // refuse new calls before the gate is released
    this.streamGate?.();
    this.streamGate = null;
    await this.withLock(async () => {
      this.transport.close();
    });
  }

  /** Serializes one call over the connection (one in-flight at a time). */
  private withLock<T>(fn: () => Promise<T>): Promise<T> {
    const run = this.chain.then(fn);
    this.chain = run.then(
      () => {},
      () => {},
    );
    return run;
  }

  /** Sends one JSON-RPC request and returns its result frame, skipping
   * pushed notifications by id correlation (§3.3). An aborted signal maps
   * to the frozen retryable TIMEOUT; a late response afterwards is skipped
   * by the next call (self-healing). */
  private async request(
    method: string,
    params?: unknown,
    signal?: AbortSignal,
  ): Promise<unknown> {
    return this.withLock(async () => {
      if (this.closed) throw McpError.unavailable();
      const id = ++this.nextId;
      const frame: Record<string, unknown> = { jsonrpc: "2.0", id, method };
      if (params !== undefined) frame.params = params;
      this.transport.write(JSON.stringify(frame));
      for (;;) {
        let line: string | null;
        try {
          line = await this.transport.readLine(signal);
        } catch (e) {
          // FRAME_TOO_LARGE poisons the stream — latch before rethrowing.
          if (e instanceof McpError && e.code === "FRAME_TOO_LARGE") this.closed = true;
          throw e;
        }
        if (line === null) {
          // The server closed (or half-closed): latch — later calls fail
          // fast with UNAVAILABLE instead of dialing a dead socket.
          this.closed = true;
          throw McpError.io("connection closed by the server");
        }
        let resp: Record<string, unknown>;
        try {
          resp = JSON.parse(line) as Record<string, unknown>;
        } catch {
          continue; // tolerate non-JSON noise frames
        }
        const rid = resp["id"] ?? 0;
        if (typeof rid !== "number") continue; // not our numeric correlation
        switch (classifyID(id, rid)) {
          case "skip":
            continue; // id-less, notification, duplicate, or late — never an error
          case "protocol":
            throw McpError.protocolError(id, rid);
          case "match":
            break;
        }
        if (resp["error"] !== undefined) {
          throw rpcError(resp["error"] as { code?: unknown; message?: string });
        }
        return resp["result"] ?? null;
      }
    });
  }

  /** Performs the MCP handshake (protocol version, client info, token)
   * and enforces the ND-12 version contract: a server older than
   * MIN_SERVER_VERSION fails fast with VERSION_MISMATCH. */
  async initialize(opts: { signal?: AbortSignal } = {}): Promise<void> {
    const params: Record<string, unknown> = {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: this.cfg.name, version: this.cfg.version },
    };
    if (this.cfg.token !== undefined) params.token = this.cfg.token;
    const raw = (await this.request("initialize", params, opts.signal)) as {
      serverInfo?: { version?: string };
    };
    const server = raw?.serverInfo?.version ?? "";
    if (versionLess(parseVersion(server), parseVersion(MIN_SERVER_VERSION))) {
      throw McpError.versionMismatch(server);
    }
  }

  /** Establishes session identity (MRFC-0040); subsequent calls inherit
   * it. On TCP the identity is server-assigned by --tcp-token, so
   * agent_id must be omitted there — only run_id is per-session. */
  async sessionInit(p: SessionParams): Promise<void> {
    await this.request("session/init", p);
  }

  /** Calls any registered MCP tool by name and returns its data payload —
   * the escape hatch for tools without a typed wrapper here. */
  async callTool(
    name: string,
    args?: unknown,
    opts: { signal?: AbortSignal } = {},
  ): Promise<unknown> {
    const params: Record<string, unknown> = { name };
    if (args !== undefined) params.arguments = args;
    const raw = (await this.request("tools/call", params, opts.signal)) as {
      content?: Array<{ text?: string }>;
    };
    const text = raw?.content?.[0]?.text ?? "";
    let payload: Record<string, unknown>;
    try {
      payload = JSON.parse(text) as Record<string, unknown>;
    } catch (e) {
      throw McpError.json(String(e));
    }
    // Mirrors the Python SDK: an absent "ok" means success; the "data"
    // field, when present, wraps the payload.
    if (payload["ok"] === false) {
      if (payload["error"] !== undefined) {
        const e = payload["error"] as {
          code?: string;
          message?: string;
          retryable?: boolean;
          suggestion?: string;
        };
        throw new McpError(
          e.code ?? "INTERNAL",
          e.message ?? "",
          e.retryable ?? false,
          e.suggestion ?? "",
        );
      }
      throw new McpError(
        "INTERNAL",
        `tool ${name} failed without an error envelope`,
        false,
        "",
      );
    }
    if (payload["data"] !== undefined) return payload["data"];
    return payload;
  }

  /** Runs a streaming query. Yields the response frame (the first data
   * chunk — for total_chunks == 1 there is no notify at all), then each
   * notify chunk until its done flag. Returning from the iterator cancels
   * the read: the connection is released for the next call. The Client
   * must not be shared while the stream is open (calls serialize on the
   * same chain) — the Go SDK's caveat, inherited. */
  async *queryStream(
    query: string,
    subject: string,
    opts: { signal?: AbortSignal } = {},
  ): AsyncGenerator<unknown, void, unknown> {
    const params: Record<string, unknown> = { query };
    if (subject !== "") params.subject = subject;
    // The stream holds the connection lock for its whole life: wait for
    // the previous holder, then gate the next caller on our return.
    let release!: () => void;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const prev = this.chain;
    this.chain = prev.then(() => gate);
    // Registered so close() can cancel a mid-stream close (§17); the
    // generator's finally clears it (identity-checked against a newer
    // stream's gate).
    this.streamGate = release;
    await prev;
    try {
      if (this.closed) throw McpError.unavailable();
      const id = ++this.nextId;
      this.transport.write(
        JSON.stringify({ jsonrpc: "2.0", id, method: "aikoql/stream", params }),
      );
      let streamId: string | null = null;
      let total = 0;
      let received = 0;
      for (;;) {
        let line: string | null;
        try {
          line = await this.transport.readLine(opts.signal);
        } catch (e) {
          if (e instanceof McpError && e.code === "FRAME_TOO_LARGE") this.closed = true;
          throw e;
        }
        if (line === null) {
          this.closed = true;
          throw McpError.io("connection closed by the server");
        }
        let resp: Record<string, unknown>;
        try {
          resp = JSON.parse(line) as Record<string, unknown>;
        } catch {
          continue;
        }
        const rid = resp["id"] ?? 0;
        if (typeof rid === "number" && rid === id) {
          if (resp["error"] !== undefined) {
            throw rpcError(resp["error"] as { code?: unknown; message?: string });
          }
          const head = (resp["result"] ?? {}) as {
            stream_id?: string;
            total_chunks?: number;
          };
          streamId = head.stream_id ?? null;
          total = head.total_chunks ?? 0;
          // The response frame IS the first chunk (it carries the data;
          // for total_chunks == 1 there is no notify at all) — the Go,
          // Python and Rust SDKs yield it too.
          yield resp["result"] ?? null;
          received += 1;
          if (total > 0 && received >= total) return;
          continue;
        }
        if (streamId === null) continue; // push before the response frame
        const p = streamNotify(resp, streamId);
        if (p === null) continue;
        yield p;
        received += 1;
        if (p.done === true || (total > 0 && received >= total)) {
          return; // the Go exit condition: done, or received == total_chunks
        }
      }
    } finally {
      if (this.streamGate === release) this.streamGate = null;
      this.transport.cancelPendingRead();
      release();
    }
  }

  // — Typed wrappers — the canonical Database API surface over the MCP
  // tools (crates/sdk/go/tools.go is the schema source).

  /** Commits a knowledge object (or a new version of one). */
  async remember(p: RememberParams): Promise<Remembered> {
    const raw = await this.callTool("remember", p);
    return raw as Remembered;
  }

  /** Fetches a knowledge object by KOID. */
  async get(koid: string, subject: string): Promise<KnowledgeObject> {
    const args: Record<string, unknown> = { koid };
    if (subject !== "") args.subject = subject;
    const raw = await this.callTool("get", args);
    return raw as KnowledgeObject;
  }

  /** Tombstones ("tombstone") or legally erases ("erase") a knowledge
   * object, audit-preserving. */
  async forget(koid: string, mode: string, subject: string): Promise<unknown> {
    const args: Record<string, unknown> = { koid, mode };
    if (subject !== "") args.subject = subject;
    return this.callTool("forget", args);
  }

  /** Runs hybrid recall and returns the scored hits. */
  async findSimilar(p: FindSimilarParams): Promise<ScoredKO[]> {
    const raw = (await this.callTool("find_similar", p)) as { results?: ScoredKO[] };
    return raw.results ?? [];
  }

  /** Runs an AikoQL query and returns the raw result rows. */
  async aikoql(query: string, subject: string): Promise<unknown> {
    const args: Record<string, unknown> = { query };
    if (subject !== "") args.subject = subject;
    return this.callTool("aikoql", args);
  }

  /** Links two knowledge objects. */
  async relate(
    fromKoid: string,
    toKoid: string,
    relType: string,
    subject: string,
  ): Promise<unknown> {
    const args: Record<string, unknown> = { from: fromKoid, to: toKoid, rel_type: relType };
    if (subject !== "") args.subject = subject;
    return this.callTool("relate", args);
  }

  /** Walks the relationship graph from a KOID. */
  async traverse(
    koid: string,
    relType: string,
    subject: string,
    depth: number,
  ): Promise<unknown> {
    const args: Record<string, unknown> = { koid, depth };
    if (relType !== "") args.rel_type = relType;
    if (subject !== "") args.subject = subject;
    return this.callTool("traverse", args);
  }

  /** Returns the server health payload. */
  async health(): Promise<unknown> {
    return this.callTool("health");
  }

  /** Returns the schema-discovery payload. */
  async discoverSchema(): Promise<unknown> {
    return this.callTool("discover_schema");
  }

  /** Returns the server's metrics. */
  async metrics(): Promise<Metrics> {
    const raw = await this.callTool("metrics");
    return raw as Metrics;
  }

  /** Returns the full lineage of a fact (versions + events). */
  async trace(koid: string, subject: string): Promise<unknown> {
    const args: Record<string, unknown> = { koid };
    if (subject !== "") args.subject = subject;
    return this.callTool("trace", args);
  }

  /** Returns the explanation payload for a KO version. */
  async explain(koid: string, subject: string, version?: number): Promise<unknown> {
    const args: Record<string, unknown> = { koid };
    if (version !== undefined) args.version = version;
    if (subject !== "") args.subject = subject;
    return this.callTool("explain", args);
  }

  /** Opens a transaction. With no id a random 32-hex id is generated;
   * passing one retries the same begin idempotently (P5-M20). */
  async begin(txnId?: string): Promise<Tx> {
    const id = txnId ?? randomBytes(16).toString("hex");
    await this.callTool("txn_begin", { txn_id: id });
    return new Tx(this, id);
  }
}

/** Runs `fn` under a response deadline: elapsed → the frozen retryable
 * TIMEOUT. A late response afterwards is harmless — id correlation skips
 * it on the next call (self-healing). */
export async function withDeadline<T>(
  ms: number,
  fn: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), ms);
  try {
    return await fn(ctrl.signal);
  } catch (e) {
    if (ctrl.signal.aborted) throw McpError.deadline();
    throw e;
  } finally {
    clearTimeout(timer);
  }
}
