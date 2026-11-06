import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { TaskRunner } from "../src/task-runner.ts";
import type { Dispatch } from "../src/task-runner.ts";
import type { AgentMessage } from "../src/protocol.ts";
import { contract } from "./fake-node.ts";

type Call = Parameters<Dispatch>[0] & { hooks: Parameters<Dispatch>[1] };

// A dispatch that records its hooks and never finishes on its own, so the test
// decides when each task ends.
function setup(dispatch?: Dispatch) {
  const sent: AgentMessage[] = [];
  const calls: Call[] = [];
  const runner = new TaskRunner({
    send: (message) => {
      sent.push(message);
    },
    dispatch:
      dispatch ??
      ((task, hooks) => {
        calls.push({ ...task, hooks });
        return new Promise<void>(() => {});
      }),
  });
  return { runner, sent, calls };
}

const task = { type: "task" as const, id: "t1", instructions: "Be brief.", text: "hi" };

test("dispatches the framed task body", () => {
  const { runner, calls } = setup();
  const [example] = contract.frame.examples;
  runner.handle({ type: "task", id: "t1", instructions: example.instructions, text: example.text });
  assert.equal(calls[0].body, example.framed);
});

test("the first final delivery sends the result and later ones are ignored", () => {
  const { runner, sent, calls } = setup();
  runner.handle(task);
  calls[0].hooks.deliver("first answer", "final");
  calls[0].hooks.deliver("second answer", "final");
  calls[0].hooks.deliver("late progress", "tool");
  calls[0].hooks.onTerminal("completed");
  assert.deepEqual(sent, [{ type: "result", id: "t1", text: "first answer" }]);
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

test("a dispatch that throws at once sends failed", async () => {
  const { runner, sent } = setup(() => {
    throw new Error("internal detail");
  });
  runner.handle(task);
  await sleep(0);
  assert.deepEqual(sent, [{ type: "failed", id: "t1" }]);
});

test("the hooks of a cancelled run do not answer a later run with the same id", () => {
  const { runner, sent, calls } = setup();
  runner.handle(task);
  runner.handle({ type: "cancel", id: "t1" });
  runner.handle(task);
  calls[0].hooks.deliver("old answer", "final");
  calls[1].hooks.deliver("new answer", "final");
  assert.deepEqual(sent, [
    { type: "cancelled", id: "t1" },
    { type: "result", id: "t1", text: "new answer" },
  ]);
});
