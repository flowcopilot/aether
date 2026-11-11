import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, existsSync, readFileSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { connectStdio, fakeAether, until } from './support.mjs';

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
  const data = path.join(dir, 'data');
  const starts = path.join(dir, 'starts');
  const results = path.join(dir, 'answer');
  const binary = fakeAether(path.join(dir, 'fake-aether'), { starts, results, taskId: 'stdio-task' });
  const install = spawnSync(process.execPath, [installer, '--data-dir', data, '--aether-bin', binary], { encoding: 'utf8' });
  assert.equal(install.status, 0, install.stdout + install.stderr);
  const { client, call } = await connectStdio(JSON.parse(install.stdout).mcpServers['aether-executor']);
  let childPid;
  t.after(async () => {
    await client.close();
    if (childPid) {
      await until(() => { try { process.kill(childPid, 0); return false; } catch { return true; } }, 'the node must stop with its bridge');
      assert.throws(() => process.kill(childPid, 0), { code: 'ESRCH' });
    }
    rmSync(dir, { recursive: true, force: true });
  });
  await until(async () => { const s = await call('bridge_status'); return s.connected && s.waiting; }, 'managed node must become connected');
  childPid = Number(readFileSync(starts, 'utf8'));
  const assignment = await call('claim_task', { claim_id: randomUUID() });
  assert.equal(assignment.id, 'stdio-task');
  const claim = { id: assignment.id, lease: assignment.lease };
  assert.equal((await call('report_progress', claim)).status, 'running');
  assert.equal((await call('finish_task', { ...claim, outcome: 'result', text: 'Blue' })).status, 'recorded');
  await until(() => existsSync(results), 'the node must receive the result');
  assert.deepEqual(JSON.parse(readFileSync(results, 'utf8')), { type: 'result', id: 'stdio-task', text: 'Blue' });
});
