// The node end of the agent socket, and the files of protocol/agent-socket.
import assert from "node:assert/strict";
import { EventEmitter, on, once } from "node:events";
import { readFileSync } from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const PROTOCOL_DIR = new URL("../../../protocol/agent-socket/", import.meta.url);
export const contract = JSON.parse(readFileSync(new URL("contract.json", PROTOCOL_DIR), "utf8"));
export const transcript = JSON.parse(readFileSync(new URL("transcript.json", PROTOCOL_DIR), "utf8"));

export async function waitFor(condition: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + 5000;
  while (!condition()) {
    assert.ok(Date.now() < deadline, what);
    await sleep(1);
  }
}

// A line of exactly `size` bytes, the newline included, of a type no side knows.
export function noise(size: number): string {
  const prefix = '{"type":"noise","pad":"';
  return prefix + "x".repeat(size - prefix.length - 3) + '"}\n';
}

export class FakeNode {
  readonly path = path.join(os.tmpdir(), `ae-${process.pid}-${Math.random().toString(36).slice(2, 8)}.sock`);
  readonly #server = net.createServer();
  readonly #connections = on(this.#server, "connection");
  #socket: net.Socket | undefined;
  #lines: string[] = [];
  #ended = false;
  readonly #changes = new EventEmitter();

  async listen(): Promise<void> {
    this.#server.listen(this.path);
    await once(this.#server, "listening");
  }

  async accept(): Promise<void> {
    const socket = (await this.#connections.next()).value[0] as net.Socket;
    this.#socket = socket;
    this.#lines = [];
    this.#ended = false;
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("error", () => {});
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      let end = buffer.indexOf("\n");
      while (end !== -1) {
        this.#lines.push(buffer.slice(0, end));
        buffer = buffer.slice(end + 1);
        end = buffer.indexOf("\n");
      }
      this.#changes.emit("change");
    });
    socket.on("close", () => {
      this.#ended = true;
      this.#changes.emit("change");
    });
  }

  write(data: string): void {
    this.#socket?.write(data);
  }

  async read(): Promise<unknown> {
    await waitFor(() => this.#lines.length > 0 || this.#ended, "the agent must send a line");
    const line = this.#lines.shift();
    assert.ok(line !== undefined, "the agent closed instead of sending a line");
    return JSON.parse(line);
  }

  async closed(): Promise<boolean> {
    await waitFor(() => this.#ended, "the agent must close the connection");
    return this.#lines.length === 0;
  }

  hangUp(): void {
    this.#socket?.end();
  }

  close(): void {
    this.#socket?.destroy();
  }

  stop(): void {
    this.close();
    this.#server.close();
  }
}
