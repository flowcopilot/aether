import assert from 'node:assert/strict';
import { test } from 'node:test';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { setTimeout as sleep } from 'node:timers/promises';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { acquireRuntime, alive } from '../src/runtime.mjs';

const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
const bridge = fileURLToPath(new URL('../src/main.mjs', import.meta.url));

function directory(t) {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'ae-runtime-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

async function until(check, message, ms = 7000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    assert.ok(Date.now() < deadline, message);
    await sleep(20);
  }
}

async function deadPid() {
  const child = spawn(process.execPath, ['-e', '']);
  await once(child, 'exit');
  return child.pid;
}

function staleLock(stateDir, owner) {
  const lock = path.join(stateDir, 'runtime.lock');
  mkdirSync(lock, { recursive: true });
  if (owner) writeFileSync(path.join(lock, 'owner.json'), JSON.stringify(owner));
  return lock;
}

test('a second bridge forwards to the owner, and becomes the owner once it is gone', async t => {
  const stateDir = directory(t);
  const owner = await acquireRuntime({ stateDir, dataDir: stateDir });
  assert.equal(owner.role, 'owner');
  t.after(() => owner.release());
  await owner.serve({
    status: () => ({ connected: true, waiting: 2, executor: 'idle' }),
    claim: () => { throw new Error('Claim is blocked'); },
    progress: async (id, lease) => ({ id, lease, status: 'running' }),
    finish: () => { throw Object.assign(new Error('/private/path'), { code: 'EACCES' }); },
  });
  const proxy = await acquireRuntime({ stateDir, dataDir: stateDir });
  assert.equal(proxy.role, 'proxy');
  t.after(() => proxy.executor.close());
  assert.deepEqual(await proxy.executor.status(), { connected: true, waiting: 2, executor: 'idle' });
  assert.deepEqual(await proxy.executor.progress('a', 'b'), { id: 'a', lease: 'b', status: 'running' });
  await assert.rejects(proxy.executor.claim(randomUUID()), { message: 'Claim is blocked' });
  // Storage errors keep their public form across the socket: no paths.
  await assert.rejects(proxy.executor.finish('a', 'b', 'result', ''), { message: /^Bridge storage unavailable/ });
  owner.release();
  await proxy.executor.closed;
  const next = await acquireRuntime({ stateDir, dataDir: stateDir });
  assert.equal(next.role, 'owner');
  next.release();
});

test('a lock left by a bridge that is gone is taken over', async t => {
  const stateDir = directory(t);
  staleLock(stateDir, { pid: await deadPid() });
  const runtime = await acquireRuntime({ stateDir, dataDir: stateDir, waitMs: 500 });
  assert.equal(runtime.role, 'owner');
  assert.equal(JSON.parse(readFileSync(path.join(stateDir, 'runtime.lock', 'owner.json'), 'utf8')).pid, process.pid);
  runtime.release();
  assert.equal(existsSync(path.join(stateDir, 'runtime.lock')), false);
});

test('taking over a stale lock stops the node its bridge left running', { skip: !existsSync('/proc/self/cmdline') }, async t => {
  const stateDir = directory(t);
  const dataDir = path.join(stateDir, 'data');
  const orphan = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)', 'node', 'serve', '--data-dir', dataDir]);
  t.after(() => orphan.kill('SIGKILL'));
  staleLock(stateDir, { pid: await deadPid(), node: orphan.pid });
  const runtime = await acquireRuntime({ stateDir, dataDir, waitMs: 500 });
  assert.equal(runtime.role, 'owner');
  assert.equal(alive(orphan.pid), false);
  runtime.release();
});

test('a live process that is not the stale bridge\'s node is never signalled', async t => {
  const stateDir = directory(t);
  const other = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)']);
  t.after(() => other.kill('SIGKILL'));
  staleLock(stateDir, { pid: await deadPid(), node: other.pid });
  const runtime = await acquireRuntime({ stateDir, dataDir: path.join(stateDir, 'data'), waitMs: 500 });
  assert.equal(runtime.role, 'owner');
  assert.equal(alive(other.pid), true);
  runtime.release();
});

test('a lock without an owner record, or whose live owner does not answer, is reported', async t => {
  const stateDir = directory(t);
  const lock = staleLock(stateDir);
  await assert.rejects(acquireRuntime({ stateDir, dataDir: stateDir, waitMs: 300 }), { message: /did not record its process/ });
  writeFileSync(path.join(lock, 'owner.json'), JSON.stringify({ pid: process.pid }));
  await assert.rejects(acquireRuntime({ stateDir, dataDir: stateDir, waitMs: 300 }), { message: new RegExp(`Bridge process ${process.pid} .* does not answer`) });
  assert.equal(existsSync(lock), true);
});

test('the exit message carries the node\'s last stderr line and the lock is released', async t => {
  const dir = directory(t);
  const binary = path.join(dir, 'fake-aether');
  const data = path.join(dir, 'data');
  writeFileSync(binary, `#!${process.execPath}
process.stderr.write('aether: starting\\n');
process.stderr.write('aether: node policy workspace /gone cannot be opened (entity not found)\\n');
process.exit(1);
`, { mode: 0o700 });
  const child = spawn(process.execPath, [bridge], {
    env: { ...process.env, AETHER_DATA_DIR: data, AETHER_BIN: binary, GROKBOT_TRANSPORT: 'stdio', GROKBOT_START_NODE: '1', GROKBOT_EXPERIMENTAL: '1' },
    stdio: ['pipe', 'ignore', 'pipe'],
  });
  t.after(() => child.kill('SIGKILL'));
  let stderr = '';
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const [code] = await once(child, 'exit');
  assert.equal(code, 1);
  assert.match(stderr, /Local Aether node exited \(code 1\): aether: node policy workspace \/gone cannot be opened \(entity not found\); closing the executor/);
  assert.equal(existsSync(path.join(data, 'grokbot-bridge', 'runtime.lock')), false);
});

test('two command connectors share one node; the survivor restarts it when the owner closes', async t => {
  const dir = directory(t);
  const binary = path.join(dir, 'fake-aether');
  const data = path.join(dir, 'data');
  const starts = path.join(dir, 'starts');
  writeFileSync(binary, `#!${process.execPath}
import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
if (process.argv.includes('--help')) { console.log('--adapter'); process.exit(0); }
fs.appendFileSync(${JSON.stringify(starts)}, process.pid + '\\n');
const socket = path.join(process.env.AETHER_DATA_DIR, 'agent.sock');
fs.rmSync(socket, { force: true });
net.createServer(peer => {
  let buffer = '';
  peer.setEncoding('utf8');
  peer.on('data', chunk => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf('\\n')) !== -1) {
      const message = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
      if (message.type === 'ready') peer.write(JSON.stringify({ type: 'task', id: 'task-' + process.pid, instructions: 'Be brief.', text: 'Name a colour.' }) + '\\n');
    }
  });
}).listen(socket);
process.on('SIGTERM', () => process.exit(0));
`, { mode: 0o700 });
  const install = spawnSync(process.execPath, [installer, '--data-dir', data, '--aether-bin', binary], { encoding: 'utf8' });
  assert.equal(install.status, 0, install.stdout + install.stderr);
  const config = JSON.parse(install.stdout).mcpServers['aether-executor'];
  const connect = async () => {
    const transport = new StdioClientTransport({ ...config, stderr: 'pipe' });
    transport.stderr?.resume();
    const client = new Client({ name: 'command-host', version: '1.0.0' });
    await client.connect(transport);
    t.after(() => client.close());
    const call = async (name, args = {}) => {
      const response = await client.callTool({ name, arguments: args });
      return response.isError ? { error: response.content[0].text } : JSON.parse(response.content[0].text);
    };
    return { client, call };
  };
  const nodes = () => existsSync(starts) ? readFileSync(starts, 'utf8').trim().split('\n').map(Number) : [];
  const first = await connect();
  await until(async () => { const s = await first.call('bridge_status'); return s.connected && s.waiting; }, 'owner node must connect');
  const second = await connect();
  // The second connector reads the owner's mailbox; it does not start a node.
  assert.deepEqual(await second.call('bridge_status'), { connected: true, waiting: 1, executor: 'idle' });
  assert.equal(nodes().length, 1);
  const assignment = await second.call('claim_task', { claim_id: randomUUID() });
  assert.equal(assignment.id, `task-${nodes()[0]}`);
  assert.equal((await first.call('finish_task', { id: assignment.id, lease: assignment.lease, outcome: 'result', text: 'Blue' })).status, 'recorded');
  await first.client.close();
  await until(() => nodes().length === 2 && !alive(nodes()[0]), 'the survivor must start a new node after the old one stops');
  await until(async () => { const s = await second.call('bridge_status'); return s.connected && s.waiting; }, 'survivor node must connect');
  assert.equal((await second.call('claim_task', { claim_id: randomUUID() })).id, `task-${nodes()[1]}`);
});
