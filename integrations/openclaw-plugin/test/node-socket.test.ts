import assert from "node:assert/strict";
import { EventEmitter, on, once } from "node:events";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { NodeSocket } from "../src/node-socket.ts";
import type { NodeMessage } from "../src/protocol.ts";

const READY = '{"type":"ready","protocol":1}';

// Lines that arrive on a socket. Lines that arrive before the next call are kept.
function lineReader(socket: net.Socket): () => Promise<string> {
  const events = new EventEmitter();
  let buffer = "";
  socket.setEncoding("utf8");
  socket.on("error", () => {});
  socket.on("data", (chunk: string) => {
    buffer += chunk;
    let end = buffer.indexOf("\n");
    while (end !== -1) {
      events.emit("line", buffer.slice(0, end));
      buffer = buffer.slice(end + 1);
      end = buffer.indexOf("\n");
    }
  });
  const lines = on(events, "line");
  return async () => (await lines.next()).value[0] as string;
}

test("says ready, reads a task split over two writes, reconnects, and stops on abort", async () => {
  const socketPath = path.join(os.tmpdir(), `ae-${process.pid}-${Math.random().toString(36).slice(2, 8)}.sock`);
  const server = net.createServer();
  const connections = on(server, "connection");
  server.listen(socketPath);
  await once(server, "listening");

  const messages = new EventEmitter();
  const received = on(messages, "message");
  const nextMessage = async () => (await received.next()).value[0] as NodeMessage;
  const statuses: boolean[] = [];
  const node = new NodeSocket({
    path: socketPath,
    retryMs: 10,
    onMessage: (message) => messages.emit("message", message),
    onStatus: (connected) => statuses.push(connected),
  });
  const nextConnection = async () => (await connections.next()).value[0] as net.Socket;
  const controller = new AbortController();
  const running = node.run(controller.signal);

  try {
    const first = await nextConnection();
    const readFirst = lineReader(first);
    assert.equal(await readFirst(), READY);

    first.write('{"type":"task","id":"t1",');
    await sleep(20);
    first.write('"instructions":"Be brief.","text":"hi"}\n');
    assert.deepEqual(await nextMessage(), { type: "task", id: "t1", instructions: "Be brief.", text: "hi" });

    // The node goes away. The agent must come back and say ready again.
    first.destroy();
    const second = await nextConnection();
    const readSecond = lineReader(second);
    assert.equal(await readSecond(), READY);
    assert.deepEqual(statuses, [true, false, true]);

    assert.equal(node.send({ type: "progress", id: "t1" }), true);
    assert.equal(await readSecond(), '{"type":"progress","id":"t1"}');

    controller.abort();
    await running;
    assert.equal(node.send({ type: "progress", id: "t1" }), false);
  } finally {
    controller.abort();
    server.close();
  }
});

test("send reports false while no node is connected", () => {
  const node = new NodeSocket({ path: "/nonexistent/agent.sock", retryMs: 10, onMessage: () => {} });
  assert.equal(node.send({ type: "failed", id: "t1" }), false);
});

test("run resolves on abort while it waits to retry", async () => {
  const node = new NodeSocket({
    path: path.join(os.tmpdir(), `ae-missing-${process.pid}.sock`),
    retryMs: 60_000,
    onMessage: () => {},
  });
  const controller = new AbortController();
  const running = node.run(controller.signal);
  await sleep(10);
  controller.abort();
  await running;
});
