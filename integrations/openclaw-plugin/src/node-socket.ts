// Stays attached to the node socket: it connects, says ready, hands each node
// message to onMessage, and connects again when the connection closes.
import net from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { MAX_LINE, PROTOCOL, RECONNECT_MS, encode, parseNodeLine } from "./protocol.ts";
import type { AgentMessage, NodeMessage } from "./protocol.ts";

export interface NodeSocketOptions {
  path: string;
  onMessage: (message: NodeMessage) => void;
  onStatus?: (connected: boolean) => void;
  retryMs?: number;
}

export class NodeSocket {
  readonly #path: string;
  readonly #onMessage: (message: NodeMessage) => void;
  readonly #onStatus: (connected: boolean) => void;
  readonly #retryMs: number;
  #socket: net.Socket | undefined;

  constructor(options: NodeSocketOptions) {
    this.#path = options.path;
    this.#onMessage = options.onMessage;
    this.#onStatus = options.onStatus ?? (() => {});
    this.#retryMs = options.retryMs ?? RECONNECT_MS;
  }

  // Resolves when the signal aborts.
  async run(signal: AbortSignal): Promise<void> {
    while (!signal.aborted) {
      await this.#session(signal);
      try {
        await sleep(this.#retryMs, undefined, { signal });
      } catch {}
    }
  }

  // False when no node is connected.
  send(message: AgentMessage): boolean {
    const socket = this.#socket;
    if (socket === undefined || socket.destroyed) {
      return false;
    }
    socket.write(encode(message));
    return true;
  }

  #session(signal: AbortSignal): Promise<void> {
    return new Promise((resolve) => {
      const socket = net.createConnection(this.#path);
      let connected = false;
      let buffer = Buffer.alloc(0);
      const stop = () => socket.destroy();
      signal.addEventListener("abort", stop, { once: true });

      socket.on("connect", () => {
        socket.write(encode({ type: "ready", protocol: PROTOCOL }));
        this.#socket = socket;
        connected = true;
        this.#onStatus(true);
      });
      socket.on("data", (chunk: Buffer) => {
        buffer = Buffer.concat([buffer, chunk]);
        let end = buffer.indexOf(0x0a);
        while (end !== -1 && end < MAX_LINE) {
          const message = parseNodeLine(buffer.subarray(0, end).toString("utf8"));
          buffer = buffer.subarray(end + 1);
          if (message !== undefined) {
            this.#onMessage(message);
          }
          end = buffer.indexOf(0x0a);
        }
        // A line, its newline included, has at most MAX_LINE bytes.
        if (end >= MAX_LINE || (end === -1 && buffer.length >= MAX_LINE)) {
          socket.destroy();
        }
      });
      // An error is always followed by close, so the cleanup runs there.
      socket.on("error", () => {});
      socket.on("close", () => {
        signal.removeEventListener("abort", stop);
        if (this.#socket === socket) {
          this.#socket = undefined;
        }
        if (connected) {
          this.#onStatus(false);
        }
        resolve();
      });
    });
  }
}
