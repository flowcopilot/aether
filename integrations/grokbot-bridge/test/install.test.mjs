import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, writeFileSync, existsSync, readFileSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { setTimeout as sleep } from 'node:timers/promises';
import { randomUUID } from 'node:crypto';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));

test('installer dry-run returns an absolute command without creating data or invoking a build', t => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'ae-install-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const data = path.join(dir, 'data with spaces');
  const result = spawnSync(process.execPath, [installer, '--dry-run', '--data-dir', data], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  const plan = JSON.parse(result.stdout);
  assert.equal(plan.status, 'planned');
  assert.equal(plan.data_dir, data);
  assert.equal(plan.mcpServers['aether-executor'].command, process.execPath);
  assert.equal(plan.mcpServers['aether-executor'].env.GROKBOT_TRANSPORT, 'stdio');
  assert.equal(plan.build.args.at(-1), path.join(data, 'tools'));
  assert.equal(existsSync(data), false);
});

test('installed command owns its node, speaks clean stdio MCP, executes a task, and stops its child', async t => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'ae-stdio-'));
  const binary = path.join(dir, 'fake-aether');
  const data = path.join(dir, 'data');
  const pidFile = path.join(dir, 'pid');
  const resultFile = path.join(dir, 'answer');
  writeFileSync(binary, `#!${process.execPath}
import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
if (process.argv.includes('--help')) { console.log('--adapter'); process.exit(0); }
fs.writeFileSync(${JSON.stringify(pidFile)}, String(process.pid));
const server = net.createServer(socket => {
  let buffer = '';
  socket.setEncoding('utf8');
  socket.on('data', chunk => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf('\\n')) !== -1) {
      const message = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
      if (message.type === 'ready') socket.write(JSON.stringify({type:'task', id:'stdio-task', instructions:'Be brief.', text:'Name a colour.'}) + '\\n');
      if (message.type === 'result') fs.writeFileSync(${JSON.stringify(resultFile)}, JSON.stringify(message));
    }
  });
});
server.listen(path.join(process.env.AETHER_DATA_DIR, 'agent.sock'));
`, { mode: 0o700 });
  const install = spawnSync(process.execPath, [installer, '--data-dir', data, '--aether-bin', binary], { encoding: 'utf8' });
  assert.equal(install.status, 0, install.stdout + install.stderr);
  const config = JSON.parse(install.stdout).mcpServers['aether-executor'];
  const transport = new StdioClientTransport({ ...config, stderr: 'pipe' });
  transport.stderr?.resume();
  const client = new Client({ name: 'command-host', version: '1.0.0' });
  let childPid;
  t.after(async () => {
    await client.close();
    if (childPid) {
      const deadline = Date.now() + 7000;
      while (Date.now() < deadline) {
        try { process.kill(childPid, 0); } catch { break; }
        await sleep(10);
      }
      assert.throws(() => process.kill(childPid, 0), { code: 'ESRCH' });
    }
    rmSync(dir, { recursive: true, force: true });
  });
  await client.connect(transport);
  const call = async (name, args = {}) => {
    const response = await client.callTool({ name, arguments: args });
    assert.ok(!response.isError, JSON.stringify(response));
    return JSON.parse(response.content[0].text);
  };
  const deadline = Date.now() + 7000;
  let status;
  do {
    status = await call('bridge_status');
    assert.ok(Date.now() < deadline, 'managed node must become connected');
    if (!status.connected || !status.waiting) await sleep(10);
  } while (!status.connected || !status.waiting);
  childPid = Number(readFileSync(pidFile, 'utf8'));
  const assignment = await call('claim_task', { claim_id: randomUUID() });
  assert.equal(assignment.id, 'stdio-task');
  const claim = { id: assignment.id, lease: assignment.lease };
  assert.equal((await call('report_progress', claim)).status, 'running');
  assert.equal((await call('finish_task', { ...claim, outcome: 'result', text: 'Blue' })).status, 'recorded');
  while (!existsSync(resultFile)) { assert.ok(Date.now() < deadline); await sleep(10); }
  assert.deepEqual(JSON.parse(readFileSync(resultFile, 'utf8')), { type: 'result', id: 'stdio-task', text: 'Blue' });
});
