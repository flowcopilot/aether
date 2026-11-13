import assert from "node:assert/strict";
import { EventEmitter, on } from "node:events";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { NodeSocket } from "../src/node-socket.ts";
import { MAX_TASKS } from "../src/protocol.ts";
import type { NodeMessage } from "../src/protocol.ts";
import { FakeNode, contract } from "./fake-node.ts";

const ready = { type: "ready", protocol: contract.protocol, max_tasks: MAX_TASKS };

test("reads a task split over two writes, reconnects, and stops on abort", async () => {
  const fake = new FakeNode();
  await fake.listen();
  const messages = new EventEmitter();
  const received = on(messages, "message");
  const nextMessage = async () => (await received.next()).value[0] as NodeMessage;
  const statuses: boolean[] = [];
  const node = new NodeSocket({
    path: fake.path,
    retryMs: 10,
    onMessage: (message) => messages.emit("message", message),
    onStatus: (connected) => statuses.push(connected),
  });
  const controller = new AbortController();
  const running = node.run(controller.signal);

  try {
    await fake.accept();
    assert.deepEqual(await fake.read(), ready);
    fake.write('{"type":"task","id":"t1",');
    await sleep(20);
    fake.write('"instructions":"Be brief.","text":"hi"}\n');
    assert.deepEqual(await nextMessage(), { type: "task", id: "t1", instructions: "Be brief.", text: "hi" });

    fake.close();
    await fake.accept();
    assert.deepEqual(await fake.read(), ready);
    assert.deepEqual(statuses, [true, false, true]);
    assert.equal(node.send({ type: "progress", id: "t1" }), true);
    assert.deepEqual(await fake.read(), { type: "progress", id: "t1" });

    controller.abort();
    await running;
    assert.equal(node.send({ type: "progress", id: "t1" }), false);
  } finally {
    controller.abort();
    fake.stop();
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
