import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { CHANNEL_ID, NODE_SENDER_ID, createDispatch } from "../src/inbound.ts";
import { TaskRunner } from "../src/task-runner.ts";
import { frameTask } from "../src/protocol.ts";
import type { AgentMessage } from "../src/protocol.ts";

// A fake of the OpenClaw channel runtime. It has only the members that
// inbound.ts uses. `run` plays the part of the agent turn: it calls the
// delivery and reply hooks that dispatch passes in.
type Run = (params: any) => Promise<void> | void;

function fakeRuntime(script: { ingress?: string; dispatchKind?: string; run?: Run } = {}) {
  const calls = {
    resolveStable: [] as any[],
    buildContext: [] as any[],
    dispatch: [] as any[],
  };
  const channelRuntime = {
    inbound: {
      ingress: {
        resolveStable: async (params: any) => {
          calls.resolveStable.push(params);
          return { ingress: { admission: script.ingress ?? "dispatch" } };
        },
      },
      buildContext: (params: any) => {
        calls.buildContext.push(params);
        return { SessionKey: params.route.routeSessionKey };
      },
      dispatch: async (params: any) => {
        calls.dispatch.push(params);
        await script.run?.(params);
        if (script.dispatchKind !== undefined) {
          return { admission: { kind: script.dispatchKind, reason: "test" }, dispatched: false };
        }
        return {
          admission: { kind: "dispatch" },
          dispatched: true,
          ctxPayload: {},
          routeSessionKey: params.route.sessionKey,
          dispatchResult: {},
        };
      },
    },
  };
  return { channelRuntime, calls };
}

// The route fake records the input and returns a session key that names the peer.
function fakeRoute(routeCalls: any[]) {
  return (input: any) => {
    routeCalls.push(input);
    return {
      agentId: "aether",
      accountId: input.accountId,
      dmScope: input.dmScope,
      sessionKey: `agent:aether:aether:${input.accountId}:direct:${input.peer.id}`,
    };
  };
}

const task = { type: "task" as const, id: "t1", instructions: "Be brief.", text: "hi" };

// Runs one task through the real TaskRunner, so the test sees the messages
// that would go to the node.
async function runTask(script: { ingress?: string; dispatchKind?: string; run?: Run } = {}) {
  const { channelRuntime, calls } = fakeRuntime(script);
  const routeCalls: any[] = [];
  const sent: AgentMessage[] = [];
  const runner = new TaskRunner({
    send: (message) => {
      sent.push(message);
    },
    dispatch: createDispatch({
      cfg: { channels: { aether: {} } } as any,
      accountId: "default",
      channelRuntime: channelRuntime as any,
      resolveRoute: fakeRoute(routeCalls) as any,
    }),
  });
  runner.handle(task);
  for (let i = 0; i < 50 && runner.activeCount > 0; i++) {
    await sleep(0);
  }
  return { sent, calls, routeCalls, activeCount: runner.activeCount };
}

test("each task gets a route keyed by its task id with the per-account-channel-peer scope", async () => {
  const { calls, routeCalls } = await runTask();
  assert.equal(routeCalls.length, 1);
  assert.equal(routeCalls[0].channel, CHANNEL_ID);
  assert.equal(routeCalls[0].accountId, "default");
  assert.deepEqual(routeCalls[0].peer, { kind: "direct", id: "t1" });
  assert.equal(routeCalls[0].dmScope, "per-account-channel-peer");

  // The ingress check and the dispatch use the session that the route chose.
  const sessionKey = "agent:aether:aether:default:direct:t1";
  assert.equal(calls.resolveStable[0].contextBinding.sessionKey, sessionKey);
  assert.equal(calls.dispatch[0].route.sessionKey, sessionKey);
  assert.equal(calls.dispatch[0].route.dmScope, "per-account-channel-peer");
});

test("admits every task as the one fixed node sender, never as the owner", async () => {
  const { calls } = await runTask();
  const admission = calls.resolveStable[0];
  assert.equal(admission.dmPolicy, "allowlist");
  assert.deepEqual(admission.allowFrom, [NODE_SENDER_ID]);
  assert.deepEqual(admission.subject, { stableId: NODE_SENDER_ID });
  assert.equal(calls.buildContext[0].sender.id, NODE_SENDER_ID);
  assert.equal(JSON.stringify(calls).includes("senderIsOwner"), false);
});

test("passes the framed task body unchanged and turns off command parsing", async () => {
  const { calls } = await runTask();
  const body = frameTask("Be brief.", "hi");
  assert.equal(calls.buildContext[0].message.body, body);
  assert.equal(calls.buildContext[0].message.commandBody, body);
  assert.equal(calls.buildContext[0].extra.CommandInterpretationSuppressed, true);
  assert.equal(calls.dispatch[0].ctxPayload.SessionKey, "agent:aether:aether:default:direct:t1");
});

test("tool and block deliveries send progress, and the final delivery sends the result", async () => {
  const { sent } = await runTask({
    run: ({ delivery }) => {
      delivery.deliver({ text: "searching" }, { kind: "tool" });
      delivery.deliver({ text: "thinking" }, { kind: "block" });
      delivery.deliver({ text: "the answer" }, { kind: "final" });
    },
  });
  assert.deepEqual(sent, [
    { type: "progress", id: "t1" },
    { type: "progress", id: "t1" },
    { type: "result", id: "t1", text: "the answer" },
  ]);
});

test("a completed outcome before the final reply does not drop the reply", async () => {
  // The agent run reports "completed" before the reply is delivered, so the
  // final text must still reach the node.
  const { sent } = await runTask({
    run: ({ delivery, replyOptions }) => {
      replyOptions.onAgentRunTerminalOutcome("completed");
      delivery.deliver({ text: "the answer" }, { kind: "final" });
    },
  });
  assert.deepEqual(sent, [{ type: "result", id: "t1", text: "the answer" }]);
});

test("a failed run outcome sends failed with no reason", async () => {
  const { sent } = await runTask({
    run: ({ replyOptions }) => {
      replyOptions.onAgentRunTerminalOutcome("failed");
    },
  });
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
});

test("a turn that finishes with no final text sends an empty result", async () => {
  const { sent } = await runTask();
  assert.deepEqual(sent, [{ type: "result", id: "t1", text: "" }]);
});

test("a task that ingress does not admit fails without a dispatch", async () => {
  const { sent, calls } = await runTask({ ingress: "drop" });
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
  assert.equal(calls.dispatch.length, 0);
});

test("a dispatch that does not admit the turn fails", async () => {
  const { sent } = await runTask({ dispatchKind: "drop" });
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
});

test("a delivery error fails the task and sends none of its text", async () => {
  const { sent } = await runTask({
    run: ({ delivery }) => {
      delivery.onError(new Error("model key sk-secret leaked"));
    },
  });
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
  assert.equal(JSON.stringify(sent).includes("sk-secret"), false);
});

test("a dispatch that throws fails the task and sends none of its error text", async () => {
  const { sent, activeCount } = await runTask({
    run: () => {
      throw new Error("internal detail");
    },
  });
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
  assert.equal(activeCount, 0);
});
