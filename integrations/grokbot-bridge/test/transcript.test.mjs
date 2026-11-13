// The bridge replays protocol/agent-socket/transcript.json as the agent.
//
// A Grok Bot claims a task only when it calls claim_task, so the replayed host
// claims a task just before it answers. A cancel in the transcript therefore
// reaches a waiting task and is answered with `cancelled`; a claimed task
// answers a cancel with `failed` instead, which bridge.test.mjs covers.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventEmitter, on, once } from 'node:events';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { Mailbox } from '../src/mailbox.mjs';
import { NodeLink, frameTask, parseNodeMessage } from '../src/node-link.mjs';
import { until } from './support.mjs';

const transcript = JSON.parse(readFileSync(
  new URL('../../../protocol/agent-socket/transcript.json', import.meta.url), 'utf8'));

// A line of exactly `size` bytes, the newline included, of a type no side knows.
function noise(size) {
  const prefix = '{"type":"noise","pad":"';
  return prefix + 'x'.repeat(size - prefix.length - 3) + '"}\n';
}

// The node end of the agent socket.
class FakeNode {
  #server = net.createServer();
  #connections = on(this.#server, 'connection');
  #socket;
  #lines = [];
  #ended = false;
  #changes = new EventEmitter();

  constructor(socketPath) { this.path = socketPath; }

  async listen() {
    this.#server.listen(this.path);
    await once(this.#server, 'listening');
  }

  async accept() {
    const socket = (await this.#connections.next()).value[0];
    this.#socket = socket;
    this.#lines = [];
    this.#ended = false;
    let buffer = '';
    socket.setEncoding('utf8');
    socket.on('error', () => {});
    socket.on('data', chunk => {
      buffer += chunk;
      let end;
      while ((end = buffer.indexOf('\n')) !== -1) {
        this.#lines.push(buffer.slice(0, end));
        buffer = buffer.slice(end + 1);
      }
      this.#changes.emit('change');
    });
    socket.on('close', () => { this.#ended = true; this.#changes.emit('change'); });
  }

  write(data) { this.#socket?.write(data); }

  async read() {
    await until(() => this.#lines.length > 0 || this.#ended, 'the agent must send a line', 5000);
    const line = this.#lines.shift();
    assert.ok(line !== undefined, 'the agent closed instead of sending a line');
    return JSON.parse(line);
  }

  async closed() {
    await until(() => this.#ended, 'the agent must close the connection', 5000);
    return this.#lines.length === 0;
  }

  hangUp() { this.#socket?.end(); }
  close() { this.#socket?.destroy(); }
  stop() { this.close(); this.#server.close(); }
}

async function replay(t, steps, outcomes) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'ae-grok-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const fake = new FakeNode(path.join(directory, 'agent.sock'));
  await fake.listen();
  let received = 0;
  let link;
  const mailbox = new Mailbox({ marker: path.join(directory, 'active-claim.json'), send: message => link.send(message) });
  link = new NodeLink(fake.path, message => { received += 1; mailbox.receive(message); },
    connected => mailbox.setConnected(connected), 10);
  const controller = new AbortController();
  const running = link.run(controller.signal);
  const claims = new Map();
  const firstTasks = new Map();
  let sent = 0;
  const allRead = () => until(() => received === sent, "the bridge must read the node's lines", 5000);

  // The Bot claims the waiting task, if there is one, before it answers.
  const claimed = id => {
    if (!claims.has(id)) {
      const assignment = mailbox.claim(randomUUID());
      if (assignment.status === 'claimed') {
        assert.equal(assignment.id, id);
        claims.set(id, assignment);
      }
    }
    return claims.get(id);
  };

  try {
    for (const step of steps) {
      if (step.agent !== undefined) {
        const message = step.agent;
        if (message.type === 'ready') {
          await fake.accept();
          // The bridge takes one task at a time and says so, as the transcript does not.
          assert.deepEqual(await fake.read(), { type: message.type, protocol: message.protocol, max_tasks: 1 });
          continue;
        }
        // The host answers only after the bridge has read every node line.
        await allRead();
        const task = claimed(message.id);
        if (task && message.type === 'progress') {
          assert.equal(mailbox.progress(task.id, task.lease).status, 'running');
        } else if (task && message.type === 'result') {
          assert.equal(mailbox.finish(task.id, task.lease, 'result', message.text).status, 'recorded');
        } else if (task && message.type === 'failed') {
          assert.equal(mailbox.finish(task.id, task.lease, 'failed').status, 'recorded');
        }
        if (step.ignored !== true) assert.deepEqual(await fake.read(), message);
      } else if (step.node !== undefined) {
        if (step.node.type === 'task' && !firstTasks.has(step.node.id)) firstTasks.set(step.node.id, step.node);
        fake.write(JSON.stringify(step.node) + '\n');
        sent += 1;
      } else if (step.node_line !== undefined) {
        fake.write(step.node_line + '\n');
        // A valid message for no open task still reaches the mailbox.
        sent += parseNodeMessage(step.node_line) === undefined ? 0 : 1;
      } else if (step.node_line_bytes !== undefined) {
        fake.write(noise(step.node_line_bytes));
      } else if (step.closes === 'node') {
        fake.close();
      } else if (step.closes === 'agent') {
        assert.ok(await fake.closed());
      }
    }
    await allRead();
    fake.hangUp();
    assert.ok(await fake.closed(), 'the bridge sent a line that the transcript does not have');
  } finally {
    controller.abort();
    await running;
    fake.stop();
  }

  for (const [id, outcome] of Object.entries(outcomes)) {
    if (outcome === 'cancelled' || outcome === 'lost') {
      // Never handed to the Bot, and no longer waiting.
      assert.equal(claims.has(id), false, id);
      assert.equal(mailbox.pending.has(id), false, id);
    } else {
      const task = firstTasks.get(id);
      assert.equal(claims.get(id)?.prompt, frameTask(task.instructions, task.text), id);
    }
  }
  assert.equal(mailbox.active, undefined);
}

// A case with more than one open task needs an agent that takes more than one.
for (const testCase of transcript.cases.filter(testCase => testCase.concurrent !== true)) {
  test(testCase.name, t => replay(t, testCase.steps, testCase.outcomes));
}
