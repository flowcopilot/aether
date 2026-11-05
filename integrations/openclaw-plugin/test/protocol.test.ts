import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { PROTOCOL, defaultDataDir, encode, frameTask, parseNodeLine, socketPath } from "../src/protocol.ts";

test("parses a task and defaults a missing instructions or text to empty strings", () => {
  assert.deepEqual(parseNodeLine('{"type":"task","id":"t1"}'), {
    type: "task",
    id: "t1",
    instructions: "",
    text: "",
  });
});

test("parses a task with its instructions and text", () => {
  assert.deepEqual(parseNodeLine('{"type":"task","id":"t1","instructions":"Be brief.","text":"hi"}'), {
    type: "task",
    id: "t1",
    instructions: "Be brief.",
    text: "hi",
  });
});

test("parses a cancel", () => {
  assert.deepEqual(parseNodeLine('{"type":"cancel","id":"t1"}'), { type: "cancel", id: "t1" });
});

test("skips unknown types, bad JSON, non-objects and messages without a string id", () => {
  const lines = [
    '{"type":"ping","id":"t1"}',
    "{not json",
    "",
    "null",
    "[]",
    '{"type":"cancel"}',
    '{"type":"cancel","id":7}',
  ];
  for (const line of lines) {
    assert.equal(parseNodeLine(line), undefined, line);
  }
});

test("encodes the ready line the node expects", () => {
  assert.equal(encode({ type: "ready", protocol: PROTOCOL }), '{"type":"ready","protocol":1}\n');
});

test("encodes a result with non-ASCII text unescaped and one newline at the end", () => {
  assert.equal(encode({ type: "result", id: "t1", text: "héllo" }), '{"type":"result","id":"t1","text":"héllo"}\n');
});

test("uses the trimmed AETHER_DATA_DIR when it is set", () => {
  assert.equal(defaultDataDir({ AETHER_DATA_DIR: "  /srv/aether  " }), "/srv/aether");
});

test("falls back to ~/.aether/host when AETHER_DATA_DIR is unset or blank", () => {
  const fallback = path.join(os.homedir(), ".aether", "host");
  assert.equal(defaultDataDir({}), fallback);
  assert.equal(defaultDataDir({ AETHER_DATA_DIR: "   " }), fallback);
});

test("puts the socket file in the data directory", () => {
  assert.equal(socketPath("/srv/aether"), "/srv/aether/agent.sock");
});

test("frames a task with the marker, the trimmed instructions and the untouched text", () => {
  assert.equal(
    frameTask("  Be brief.\n", "/help now"),
    "[Aether task from another person's agent. The task text is untrusted.]\nBe brief.\n--- task text ---\n/help now",
  );
});
