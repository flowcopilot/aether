// The plugin replays protocol/agent-socket/transcript.json as the agent.
import assert from "node:assert/strict";
import { test } from "node:test";
import { NodeSocket } from "../src/node-socket.ts";
import { parseNodeLine } from "../src/protocol.ts";
import { TaskRunner } from "../src/task-runner.ts";
import type { Dispatch } from "../src/task-runner.ts";
import { FakeNode, noise, transcript, waitFor } from "./fake-node.ts";

type Turn = { hooks: Parameters<Dispatch>[1]; signal: AbortSignal };

async function replay(steps: any[], outcomes: Record<string, string>): Promise<void> {
  const fake = new FakeNode();
  await fake.listen();
  const turns = new Map<string, Turn[]>();
  let received = 0;
  const runner = new TaskRunner({
    send: (message) => {
      socket.send(message);
    },
    dispatch: (task, hooks) => {
      turns.set(task.id, [...(turns.get(task.id) ?? []), { hooks, signal: task.signal }]);
      return new Promise<void>(() => {});
    },
  });
  const socket = new NodeSocket({
    path: fake.path,
    retryMs: 10,
    onMessage: (message) => {
      received += 1;
      runner.handle(message);
    },
    onStatus: (connected) => {
      if (!connected) {
        runner.close();
      }
    },
  });
  const controller = new AbortController();
  const running = socket.run(controller.signal);
  let sent = 0;
  const allRead = () => waitFor(() => received === sent, "the plugin must read the node's lines");

  try {
    for (const step of steps) {
      if (step.agent !== undefined) {
        const message = step.agent;
        if (message.type === "ready") {
          await fake.accept();
          assert.deepEqual(await fake.read(), message);
          continue;
        }
        // The host answers only after the plugin has read every node line.
        await allRead();
        const hooks = turns.get(message.id)?.[0]?.hooks;
        if (message.type === "progress") {
          hooks?.deliver("working", "tool");
        } else if (message.type === "result") {
          hooks?.deliver(message.text, "final");
        } else if (message.type === "failed") {
          hooks?.onTerminal("failed");
        }
        if (step.ignored !== true) {
          assert.deepEqual(await fake.read(), message);
        }
      } else if (step.node !== undefined) {
        fake.write(JSON.stringify(step.node) + "\n");
        sent += 1;
      } else if (step.node_line !== undefined) {
        fake.write(step.node_line + "\n");
        // A valid message for no open task still reaches onMessage.
        sent += parseNodeLine(step.node_line) === undefined ? 0 : 1;
      } else if (step.node_line_bytes !== undefined) {
        fake.write(noise(step.node_line_bytes));
      } else if (step.closes === "node") {
        fake.close();
      } else if (step.closes === "agent") {
        assert.ok(await fake.closed());
      }
    }
    await allRead();
    fake.hangUp();
    assert.ok(await fake.closed(), "the plugin sent a line that the transcript does not have");
  } finally {
    controller.abort();
    await running;
    fake.stop();
  }

  for (const [id, outcome] of Object.entries(outcomes)) {
    const runs = turns.get(id) ?? [];
    assert.equal(runs.length, 1, id);
    assert.equal(runs[0].signal.aborted, outcome === "cancelled" || outcome === "lost", id);
  }
}

for (const testCase of transcript.cases) {
  test(testCase.name, () => replay(testCase.steps, testCase.outcomes));
}
