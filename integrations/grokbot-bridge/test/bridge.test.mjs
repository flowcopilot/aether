import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import net from 'node:net';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';
import { setTimeout as sleep } from 'node:timers/promises';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { Mailbox } from '../src/mailbox.mjs';
import { NodeLink, contract, frameTask } from '../src/node-link.mjs';
import { createBridgeServer } from '../src/server.mjs';

async function until(check) {
  const deadline = Date.now() + 3000;
  while (!check()) {
    assert.ok(Date.now() < deadline, 'timed out waiting for protocol outcome');
    await sleep(2);
  }
}

async function fixture(t, options = {}) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'ae-grok-'));
  const socketPath = path.join(directory, 'agent.sock');
  const marker = path.join(directory, 'active-claim.json');
  const node = net.createServer();
  const accepted = once(node, 'connection');
  node.listen(socketPath);
  await once(node, 'listening');
  const lines = [];
  let link;
  const mailbox = new Mailbox({ marker, send: message => link.send(message), ...options });
  link = new NodeLink(socketPath, message => mailbox.receive(message), value => mailbox.setConnected(value));
  const controller = new AbortController();
  const running = link.run(controller.signal);
  const [socket] = await accepted;
  let buffer = '';
  socket.setEncoding('utf8');
  socket.on('data', chunk => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf('\n')) >= 0) {
      lines.push(JSON.parse(buffer.slice(0, end)));
      buffer = buffer.slice(end + 1);
    }
  });
  const token = 'test-only-credential-'.repeat(3);
  const server = createBridgeServer({ mailbox, token });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const url = new URL(`http://127.0.0.1:${server.address().port}/mcp`);
  const client = new Client({ name: 'grok-test', version: '1.0.0' });
  t.after(async () => {
    await client.close();
    controller.abort();
    socket.destroy();
    server.closeAllConnections();
    await Promise.all([running, new Promise(resolve => server.close(resolve)), new Promise(resolve => node.close(resolve))]);
    rmSync(directory, { recursive: true, force: true });
  });
  await client.connect(new StreamableHTTPClientTransport(url, { requestInit: { headers: { Authorization: `Bearer ${token}` } } }));
  await until(() => lines.length > 0);
  assert.deepEqual(lines.shift(), { type: 'ready', protocol: contract.protocol });
  const call = async (name, args = {}) => {
    const response = await client.callTool({ name, arguments: args });
    if (response.isError) return { error: response.content[0].text };
    return JSON.parse(response.content[0].text);
  };
  const enqueue = async id => {
    socket.write(JSON.stringify({ type: 'task', id, instructions: '  Be brief.\n', text: '/help now' }) + '\n');
    await until(() => mailbox.pending.has(id));
  };
  return { mailbox, marker, lines, socket, call, enqueue, url, token };
}

test('socket task → authenticated MCP claim/progress/result; duplicate completion cannot reply twice', async t => {
  const f = await fixture(t);
  for (const example of contract.frame.examples) assert.equal(frameTask(example.instructions, example.text), example.framed);
  await f.enqueue('task-1');
  const args = { claim_id: randomUUID() };
  const assignment = await f.call('claim_task', args);
  assert.equal(assignment.prompt, contract.frame.examples[0].framed);
  assert.equal((await f.call('claim_task', args)).lease, assignment.lease);
  assert.equal((await f.call('claim_task', { claim_id: randomUUID() })).status, 'blocked');
  const claim = { id: assignment.id, lease: assignment.lease };
  assert.match((await f.call('report_progress', { ...claim, lease: randomUUID() })).error, /Unknown/);
  assert.equal((await f.call('report_progress', claim)).status, 'running');
  const result = { ...claim, outcome: 'result', text: 'A useful answer' };
  assert.ok((await f.call('finish_task', { ...result, text: 'é'.repeat(17 * 1024) })).error);
  assert.equal((await f.call('finish_task', result)).status, 'recorded');
  assert.equal((await f.call('finish_task', result)).status, 'recorded');
  assert.match((await f.call('finish_task', { ...result, text: 'Different' })).error, /Conflicting/);
  await until(() => f.lines.length === 2);
  assert.deepEqual(f.lines, [{ type: 'progress', id: 'task-1' }, { type: 'result', id: 'task-1', text: 'A useful answer' }]);
  assert.equal(existsSync(f.marker), false);
});

test('cancelled claimed work blocks new execution and rejects late results until stop acknowledgement', async t => {
  const f = await fixture(t);
  await f.enqueue('old');
  const task = await f.call('claim_task', { claim_id: randomUUID() });
  const claim = { id: task.id, lease: task.lease };
  f.socket.write('{"type":"cancel","id":"old"}\n');
  await until(() => f.mailbox.active.stopped);
  await f.enqueue('next');
  assert.equal((await f.call('finish_task', { ...claim, outcome: 'result', text: 'Late' })).status, 'stop_required');
  assert.equal((await f.call('report_progress', claim)).status, 'stop_required');
  assert.equal((await f.call('claim_task', { claim_id: randomUUID() })).status, 'blocked');
  assert.equal((await f.call('finish_task', { ...claim, outcome: 'stopped' })).status, 'recorded');
  assert.equal((await f.call('claim_task', { claim_id: randomUUID() })).id, 'next');
  await until(() => f.lines.length > 0);
  assert.deepEqual(f.lines, [{ type: 'failed', id: 'old' }]);
});

test('disconnect and process restart preserve the stop fence without replaying task text', async t => {
  const f = await fixture(t);
  await f.enqueue('lost');
  const task = await f.call('claim_task', { claim_id: randomUUID() });
  f.socket.destroy();
  await until(() => !f.mailbox.connected);
  const writes = [];
  const recovered = new Mailbox({ marker: f.marker, send: value => { writes.push(value); return true; } });
  recovered.setConnected(true);
  assert.equal(recovered.claim(randomUUID()).status, 'blocked');
  assert.equal(recovered.progress(task.id, task.lease).status, 'stop_required');
  assert.equal(recovered.finish(task.id, task.lease, 'result', 'Late').status, 'stop_required');
  recovered.finish(task.id, task.lease, 'stopped');
  assert.equal(recovered.claim(randomUUID()).status, 'empty');
  assert.deepEqual(writes, []);
});

test('lease expiry fails closed; queued cancellation never executes', async t => {
  const f = await fixture(t, { leaseMs: 25 });
  await f.enqueue('expired');
  const task = await f.call('claim_task', { claim_id: randomUUID() });
  await sleep(40);
  assert.equal((await f.call('report_progress', { id: task.id, lease: task.lease })).status, 'stop_required');
  await f.enqueue('queued');
  f.socket.write('{"type":"cancel","id":"queued"}\n');
  await until(() => !f.mailbox.pending.has('queued'));
  await until(() => f.lines.length === 2);
  assert.deepEqual(f.lines, [{ type: 'failed', id: 'expired' }, { type: 'cancelled', id: 'queued' }]);
});

test('waiting work is bounded, expires without execution, and malformed socket input is skipped', async t => {
  const f = await fixture(t, { capacity: 1, taskMs: 100 });
  f.socket.write('invalid JSON\n[]\n{"type":"task","id":"missing-fields"}\n');
  await f.enqueue('waiting');
  f.socket.write('{"type":"task","id":"overflow","instructions":"","text":""}\n');
  await until(() => f.lines.length === 1);
  assert.deepEqual(f.lines, [{ type: 'failed', id: 'overflow' }]);
  await sleep(120);
  assert.equal((await f.call('claim_task', { claim_id: randomUUID() })).status, 'empty');
  await until(() => f.lines.length === 2);
  assert.deepEqual(f.lines[1], { type: 'failed', id: 'waiting' });
});

test('HTTP rejects missing/wrong authority and browser origins; MCP exposes only executor tools', async t => {
  const f = await fixture(t);
  for (const authorization of [undefined, 'Bearer wrong']) {
    const res = await fetch(f.url, { method: 'POST', headers: authorization ? { Authorization: authorization } : {} });
    assert.equal(res.status, 401);
  }
  const headers = { Authorization: `Bearer ${f.token}`, Origin: 'https://evil.example' };
  assert.equal((await fetch(f.url, { method: 'POST', headers })).status, 403);
  assert.equal((await fetch(f.url, { headers: { Authorization: `Bearer ${f.token}` } })).status, 405);
  const hostileHostStatus = await new Promise((resolve, reject) => {
    const req = http.request(f.url, { method: 'POST', headers: {
      Host: 'evil.example', Authorization: `Bearer ${f.token}`,
    } }, res => { res.resume(); resolve(res.statusCode); });
    req.on('error', reject);
    req.end();
  });
  assert.equal(hostileHostStatus, 403);
  assert.equal((await fetch(f.url, { method: 'POST', headers: {
    Authorization: `Bearer ${f.token}`, 'Content-Type': 'application/json',
  }, body: 'x'.repeat(256 * 1024 + 1) })).status, 413);
  const response = await fetch(f.url, { method: 'POST', headers: {
    Authorization: `Bearer ${f.token}`, 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream',
  }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' }) });
  const body = await response.json();
  assert.deepEqual(body.result.tools.map(tool => tool.name).sort(), ['bridge_status', 'claim_task', 'finish_task', 'report_progress']);
  assert.ok((await f.call('claim_task', { claim_id: 'invalid' })).error);
});
