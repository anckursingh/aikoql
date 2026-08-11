// Aikoql TypeScript SDK — MCP JSON-RPC client over TCP.
// Usage:
//   const db = new AikoqlClient({ host: "127.0.0.1", port: 9090 });
//   await db.initialize();
//   const ko = await db.remember({ type_name: "fact", properties: { x: 1 } });

import * as net from "node:net";

// ---- Types ----------------------------------------------------------------

export interface ClientConfig {
  host: string;
  port: number;
}

export interface RememberParams {
  subject?: string;
  type_name: string;
  koid?: string;
  properties?: Record<string, unknown>;
  semantic?: Record<string, unknown>;
  expected_version?: number;
  idempotency_key?: string;
  note?: string;
}

export interface Remembered {
  koid: string;
  version: number;
  commit_ts: number;
}

export interface KnowledgeObject {
  koid: string;
  version: number;
  commit_ts: number;
  metadata: { type_name: string; schema_version: number };
  properties: Record<string, unknown>;
  lifecycle: { state: string };
}

export interface ScoredKO {
  koid: string;
  score: number;
  type_name: string;
  version: number;
}

export interface LineageEntry {
  koid: string;
  version: number;
  commit_ts: number;
  origin: string;
  note?: string;
}

export interface Metrics {
  journal_seq: number;
  total_objects: number;
  active_objects: number;
  uptime_seconds: number;
  by_lifecycle: Record<string, number>;
  by_type: Record<string, number>;
}

// ---- Client ---------------------------------------------------------------

export class AikoqlClient {
  private socket: net.Socket | null = null;
  private buffer = "";
  private nextId = 0;
  private pending = new Map<number, (result: unknown) => void>();
  private config: ClientConfig;

  constructor(config: ClientConfig) {
    this.config = config;
  }

  async connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.socket = net.createConnection(this.config.port, this.config.host, () => resolve());
      this.socket.on("error", reject);
      this.socket.on("data", (chunk: Buffer) => {
        this.buffer += chunk.toString();
        const lines = this.buffer.split("\n");
        this.buffer = lines.pop() ?? "";
        for (const line of lines) {
          if (!line.trim()) continue;
          try {
            const frame = JSON.parse(line);
            if (frame.id && this.pending.has(frame.id)) {
              this.pending.get(frame.id)!(frame.result ?? frame);
              this.pending.delete(frame.id);
            }
          } catch { /* skip parse errors */ }
        }
      });
    });
  }

  async initialize(): Promise<void> {
    await this.request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "aikoql-ts-sdk", version: "0.1.0" },
    });
  }

  close(): void {
    this.socket?.destroy();
    this.socket = null;
  }

  // ---- MCP request/response -----------------------------------------------

  private request(method: string, params: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = ++this.nextId;
      this.pending.set(id, resolve);
      const frame = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
      this.socket?.write(frame, (err) => { if (err) reject(err); });
    });
  }

  private async callTool(name: string, args: Record<string, unknown>): Promise<unknown> {
    const resp = (await this.request("tools/call", { name, arguments: args })) as {
      content?: Array<{ text: string }>;
      isError?: boolean;
    };
    if (resp.isError) throw new Error(resp.content?.[0]?.text ?? "tool error");
    return JSON.parse(resp.content?.[0]?.text ?? "null");
  }

  // ---- Knowledge Object tools ---------------------------------------------

  async remember(params: RememberParams): Promise<Remembered> {
    const { subject, ...rest } = params;
    return this.callTool("remember", {
      subject: subject ?? "sdk-user",
      ...rest,
    }) as Promise<Remembered>;
  }

  async forget(koid: string, mode: "tombstone" | "erase" = "tombstone"): Promise<unknown> {
    return this.callTool("forget", { koid, mode });
  }

  async evolve(koid: string, to: string): Promise<unknown> {
    return this.callTool("evolve", { koid, to });
  }

  async get(koid: string, subject?: string): Promise<KnowledgeObject> {
    return this.callTool("get", { subject: subject ?? "sdk-user", koid }) as Promise<KnowledgeObject>;
  }

  async findSimilar(params: {
    text?: string;
    vector?: number[];
    embedding_model?: string;
    k?: number;
    fusion?: string;
    type_name?: string;
    subject?: string;
  }): Promise<ScoredKO[]> {
    const result = (await this.callTool("find_similar", {
      subject: params.subject ?? "sdk-user",
      ...params,
    })) as { results?: ScoredKO[] };
    return result.results ?? [];
  }

  async trace(koid: string): Promise<LineageEntry[]> {
    const result = (await this.callTool("trace", { koid })) as { versions?: LineageEntry[] };
    return result.versions ?? [];
  }

  async explain(koid: string, version?: number): Promise<unknown> {
    return this.callTool("explain", { koid, ...(version ? { version } : {}) });
  }

  async prove(koid: string): Promise<unknown> {
    return this.callTool("prove", { koid });
  }

  async relate(from: string, to: string, rel_type: string): Promise<unknown> {
    return this.callTool("relate", { from, to, rel_type });
  }

  async traverse(koid: string, rel_type?: string, depth?: number): Promise<unknown> {
    return this.callTool("traverse", { koid, ...(rel_type ? { rel_type } : {}), ...(depth ? { depth } : {}) });
  }

  // ---- aikoql --------------------------------------------------------------

  async aikoql(query: string, subject?: string): Promise<unknown> {
    return this.callTool("aikoql", { query, subject: subject ?? "sdk-user" });
  }

  // ---- Ops tools -----------------------------------------------------------

  async backup(): Promise<{ backup: string; timestamp: number; verified: boolean }> {
    return this.callTool("backup", {}) as Promise<{ backup: string; timestamp: number; verified: boolean }>;
  }

  async restore(backup: string): Promise<unknown> {
    return this.callTool("restore", { backup });
  }

  async listBackups(): Promise<{ backups: Array<{ name: string; meta: unknown }> }> {
    return this.callTool("list_backups", {}) as Promise<{ backups: Array<{ name: string; meta: unknown }> }>;
  }

  async verifyBackup(backup: string): Promise<unknown> {
    return this.callTool("verify_backup", { backup });
  }

  async metrics(): Promise<Metrics> {
    return this.callTool("metrics", {}) as Promise<Metrics>;
  }

  // ---- Evals ---------------------------------------------------------------

  async evalRecall(params: {
    type_name: string;
    text?: string;
    vector?: number[];
    k?: number;
    fusion?: string;
    expected: string[];
  }): Promise<unknown> {
    return this.callTool("eval_recall", params);
  }
}
