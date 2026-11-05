// The agent side of the node socket. It connects, says ready, reads task
// lines, and reconnects when the node goes away. This follows AgentConnection
// and the reconnect loop in integrations/hermes-plugin.
import net from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { PROTOCOL, encode, parseNodeLine } from "./protocol.ts";
import type { AgentMessage, NodeMessage } from "./protocol.ts";

// Same limit as node.py. A longer line closes the connection.
const MAX_LINE = 4 * 1024 * 1024;

export interface NodeSocketOptions {
  path: string;
  onMessage: (message: NodeMessage) => void;
  onStatus?: (connected: boolean) => void;
  retryMs?: number;
  // Tests inject a connect function. Production uses the default.
  connect?: (path: string) => net.Socket;
}

export class NodeSocket {
  readonly #path: string;
  readonly #onMessage: (message: NodeMessage) => void;
  readonly #onStatus: (connected: boolean) => void;
  readonly #retryMs: number;
  readonly #connect: (path: string) => net.Socket;
  #socket: net.Socket | undefined;

  constructor(options: NodeSocketOptions) {
    this.#path = options.path;
    this.#onMessage = options.onMessage;
    this.#onStatus = options.onStatus ?? (() => {});
    this.#retryMs = options.retryMs ?? 2000;
    this.#connect = options.connect ?? ((socketPath) => net.createConnection(socketPath));
  }

  // Stay attached until the signal aborts. A closed or failed connection is
  // retried after retryMs.
  async run(signal: AbortSignal): Promise<void> {
    while (!signal.aborted) {
      await this.#session(signal);
      try {
        await sleep(this.#retryMs, undefined, { signal });
      } catch {
        // Aborted during the wait. The loop ends.
      }
    }
  }

  // False when no node is connected. The caller then has nothing to send to.
  send(message: AgentMessage): boolean {
    const socket = this.#socket;
    if (socket === undefined || socket.destroyed) {
      return false;
    }
    socket.write(encode(message));
    return true;
  }

  // One connection, from connect to close.
  #session(signal: AbortSignal): Promise<void> {
    return new Promise((resolve) => {
      const socket = this.#connect(this.#path);
      let connected = false;
      let buffer = "";
      const stop = () => socket.destroy();
      signal.addEventListener("abort", stop, { once: true });

      socket.setEncoding("utf8");
      socket.on("connect", () => {
        socket.write(encode({ type: "ready", protocol: PROTOCOL }));
        this.#socket = socket;
        connected = true;
        this.#onStatus(true);
      });
      socket.on("data", (chunk: string) => {
        buffer += chunk;
        let end = buffer.indexOf("\n");
        while (end !== -1) {
          const message = parseNodeLine(buffer.slice(0, end));
          buffer = buffer.slice(end + 1);
          if (message !== undefined) {
            this.#onMessage(message);
          }
          end = buffer.indexOf("\n");
        }
        if (buffer.length > MAX_LINE) {
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
