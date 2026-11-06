import assert from "node:assert/strict";
import { test } from "node:test";
import {
  MAX_LINE,
  PROTOCOL,
  RECONNECT_MS,
  SOCKET_FILE,
  defaultDataDir,
  encode,
  frameTask,
  parseNodeLine,
} from "../src/protocol.ts";
import { contract } from "./fake-node.ts";

test("the constants are the contract", () => {
  assert.equal(PROTOCOL, contract.protocol);
  assert.equal(SOCKET_FILE, contract.socket_file);
  assert.equal(MAX_LINE, contract.max_line_bytes);
  assert.equal(RECONNECT_MS, contract.reconnect_seconds * 1000);
});

test("the default data dir follows the contract", () => {
  for (const example of contract.data_dir.examples) {
    const env = example.env === null ? {} : { [contract.data_dir.env]: example.env };
    assert.equal(defaultDataDir(env, example.home), example.data_dir, JSON.stringify(example));
  }
});

test("a task is framed as the contract shows", () => {
  for (const example of contract.frame.examples) {
    assert.equal(frameTask(example.instructions, example.text), example.framed);
  }
});

test("parses a task and defaults a missing instructions or text to empty strings", () => {
  assert.deepEqual(parseNodeLine('{"type":"task","id":"t1"}'), {
    type: "task",
    id: "t1",
    instructions: "",
    text: "",
  });
});

test("encodes a result with non-ASCII text unescaped and one newline at the end", () => {
  assert.equal(encode({ type: "result", id: "t1", text: "héllo" }), '{"type":"result","id":"t1","text":"héllo"}\n');
});
