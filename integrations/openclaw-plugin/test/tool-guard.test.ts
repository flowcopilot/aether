import assert from "node:assert/strict";
import { test } from "node:test";
import { decideToolCall } from "../src/tool-guard.ts";

const reason = "This tool is not allowed for Aether tasks.";

test("blocks a tool outside the allowlist in the aether channel", () => {
  assert.deepEqual(decideToolCall({ channel: "aether", toolName: "exec", allowedTools: ["read"] }), {
    block: true,
    reason,
  });
});

test("allows a tool on the allowlist in the aether channel", () => {
  assert.deepEqual(decideToolCall({ channel: "aether", toolName: "read", allowedTools: ["read"] }), {
    block: false,
  });
});

test("finds an Aether session from its key when the channel is not set", () => {
  assert.deepEqual(
    decideToolCall({ sessionKey: "agent:main:aether:default:direct:t1", toolName: "exec", allowedTools: [] }),
    { block: true, reason },
  );
});

test("matches allowed tool names exactly", () => {
  for (const toolName of ["read_secret", "Read"]) {
    assert.deepEqual(decideToolCall({ channel: "aether", toolName, allowedTools: ["read"] }), {
      block: true,
      reason,
    });
  }
});

test("allows every tool outside Aether sessions", () => {
  assert.deepEqual(
    decideToolCall({ channel: "telegram", sessionKey: "agent:main:main", toolName: "exec", allowedTools: [] }),
    { block: false },
  );
});
