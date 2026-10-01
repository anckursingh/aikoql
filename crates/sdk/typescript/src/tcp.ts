// One line-framed MCP connection. The browser transport ("where
// applicable", §25) implements the same Transport surface over WebSocket
// (or a fetch duplex); nothing above this file is Node-specific — the
// D-15 native protocol replacement likewise swaps only the Transport.

import net from "node:net";
import { McpError } from "./error.ts";

/** One newline-framed byte pipe: write a frame, read lines in order. */
export interface Transport {
  write(line: string): void;
  /**
   * The next line, or null when the peer closed. An aborted signal rejects
   * with the frozen retryable TIMEOUT and leaves the stream undisturbed —
   * the abandoned line (if any) still lands in the queue for the next
   * reader (id correlation skips it: self-healing).
   */
  readLine(signal?: AbortSignal): Promise<string | null>;
  /** Cancels the one pending read (a dropped stream); the next line queues. */
  cancelPendingRead(): void;
  close(): void;
}

export class TcpTransport implements Transport {
  private socket: net.Socket;
  private buffer = "";
  private lines: string[] = [];
  // Exactly one reader at a time (the client serializes calls on a lock).
  private current: { resolve: (line: string | null) => void } | null = null;
  private closed = false;

  private constructor(socket: net.Socket) {
    this.socket = socket;
    socket.on("data", (chunk: Buffer) => {
      this.buffer += chunk.toString("utf8");
      let idx: number;
      while ((idx = this.buffer.indexOf("\n")) >= 0) {
        const line = this.buffer.slice(0, idx);
        this.buffer = this.buffer.slice(idx + 1);
        this.deliver(line);
      }
    });
    socket.on("close", () => {
      this.closed = true;
      if (this.current) {
        this.current.resolve(null);
        this.current = null;
      }
    });
    // Errors surface as close (readLine → null → "connection closed").
    socket.on("error", () => {});
  }

  /** Connects, bounding the dial by the frozen TIMEOUT (the Go/Rust shape). */
  static connect(addr: string, timeoutMs: number): Promise<TcpTransport> {
    return new Promise((resolve, reject) => {
      const [host, port] = addr.split(":");
      const socket = net.connect(Number(port), host!);
      const timer = setTimeout(() => {
        socket.destroy();
        reject(McpError.deadline());
      }, timeoutMs);
      socket.once("connect", () => {
        clearTimeout(timer);
        resolve(new TcpTransport(socket));
      });
      socket.once("error", (err: Error) => {
        clearTimeout(timer);
        reject(McpError.io(err.message));
      });
    });
  }

  private deliver(line: string): void {
    if (this.current) {
      const waiter = this.current;
      this.current = null;
      waiter.resolve(line);
    } else {
      this.lines.push(line);
    }
  }

  write(line: string): void {
    this.socket.write(line + "\n");
  }

  readLine(signal?: AbortSignal): Promise<string | null> {
    if (this.lines.length > 0) return Promise.resolve(this.lines.shift()!);
    if (this.closed) return Promise.resolve(null);
    if (signal?.aborted) return Promise.reject(McpError.deadline());
    if (this.current) throw new Error("aikoql: concurrent readers on one transport");
    return new Promise((resolve, reject) => {
      const entry = { resolve };
      const onAbort = () => {
        if (this.current === entry) {
          this.current = null;
          reject(McpError.deadline());
        }
        signal!.removeEventListener("abort", onAbort);
      };
      if (signal) signal.addEventListener("abort", onAbort, { once: true });
      this.current = entry;
    });
  }

  cancelPendingRead(): void {
    if (this.current) {
      this.current.resolve(null);
      this.current = null;
    }
  }

  close(): void {
    this.socket.destroy();
  }
}
