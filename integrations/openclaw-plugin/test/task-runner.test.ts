import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { TaskRunner } from "../src/task-runner.ts";
import type { Dispatch } from "../src/task-runner.ts";
import type { AgentMessage } from "../src/protocol.ts";
import { frameTask } from "../src/protocol.ts";

type Hooks = Parameters<Dispatch>[1];

// A dispatch that records its hooks and never finishes on its own, so the test
// decides when each task ends.
function setup(dispatch?: Dispatch) {
  const sent: AgentMessage[] = [];
  const calls: { task: { id: string; body: string }; hooks: Hooks }[] = [];
  const runner = new TaskRunner({
    send: (message) => {
      sent.push(message);
    },
    dispatch:
      dispatch ??
      ((task, hooks) => {
        calls.push({ task, hooks });
        return new Promise<void>(() => {});
      }),
  });
  return { runner, sent, calls };
}

const task = { type: "task" as const, id: "t1", instructions: "Be brief.", text: "hi" };

test("dispatches the framed task body", () => {
  const { runner, calls } = setup();
  runner.handle(task);
  assert.deepEqual(calls.map((call) => call.task), [
    { id: "t1", body: frameTask("Be brief.", "hi") },
  ]);
});

test("tool and block deliveries send progress with no text", () => {
  const { runner, sent, calls } = setup();
  runner.handle(task);
  calls[0].hooks.deliver("searching", "tool");
  calls[0].hooks.deliver("thinking", "block");
  assert.deepEqual(sent, [
    { type: "progress", id: "t1" },
    { type: "progress", id: "t1" },
  ]);
  assert.equal(runner.activeCount, 1);
});

test("the first final delivery sends the result and later ones are ignored", () => {
  const { runner, sent, calls } = setup();
  runner.handle(task);
  calls[0].hooks.deliver("first answer", "final");
  calls[0].hooks.deliver("second answer", "final");
  calls[0].hooks.deliver("late progress", "tool");
  calls[0].hooks.onTerminal("completed");
  assert.deepEqual(sent, [{ type: "result", id: "t1", text: "first answer" }]);
  assert.equal(runner.activeCount, 0);
});

test("a failed terminal outcome sends failed", () => {
  const { runner, sent, calls } = setup();
  runner.handle(task);
  calls[0].hooks.onTerminal("failed");
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
  assert.equal(runner.activeCount, 0);
});

test("a completed turn with no final text sends an empty result", () => {
  const { runner, sent, calls } = setup();
  runner.handle(task);
  calls[0].hooks.onTerminal("completed");
  assert.deepEqual(sent, [{ type: "result", id: "t1", text: "" }]);
});

test("a rejected dispatch sends failed and never the error text", async () => {
  const { runner, sent } = setup(async () => {
    throw new Error("secret detail");
  });
  runner.handle(task);
  await sleep(0);
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
});

test("a cancel sends cancelled at once and drops the late replies", () => {
  const { runner, sent, calls } = setup();
  runner.handle(task);
  runner.handle({ type: "cancel", id: "t1" });
  calls[0].hooks.deliver("late answer", "final");
  calls[0].hooks.onTerminal("completed");
  assert.deepEqual(sent, [{ type: "cancelled", id: "t1" }]);
  assert.equal(runner.activeCount, 0);
});

test("a cancel for a task that is not active sends nothing", () => {
  const { runner, sent } = setup();
  runner.handle({ type: "cancel", id: "unknown" });
  assert.deepEqual(sent, []);
});

test("a duplicate of an active task is ignored", () => {
  const { runner, calls } = setup();
  runner.handle(task);
  runner.handle(task);
  assert.equal(calls.length, 1);
  assert.equal(runner.activeCount, 1);
});
