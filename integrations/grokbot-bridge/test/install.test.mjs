import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, mkdirSync, existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
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

test('an installed binary without connected-adapter support is rebuilt once', t => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'ae-install-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const data = path.join(dir, 'data');
  const bin = path.join(dir, 'bin');
  const calls = path.join(dir, 'cargo-calls');
  mkdirSync(path.join(data, 'tools', 'bin'), { recursive: true });
  mkdirSync(bin);
  fakeAether(path.join(data, 'tools', 'bin', 'aether'), { adapter: false });
  // Stands in for `cargo install --root <tools>`: installs a current binary.
  writeFileSync(path.join(bin, 'cargo'), `#!${process.execPath}
import fs from 'node:fs';
import path from 'node:path';
import { fakeAether } from ${JSON.stringify(new URL('./support.mjs', import.meta.url).href)};
const args = process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(calls)}, JSON.stringify(args) + '\\n');
fakeAether(path.join(args[args.indexOf('--root') + 1], 'bin', 'aether'), {});
`, { mode: 0o700 });
  const env = { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}` };
  const result = spawnSync(process.execPath, [installer, '--data-dir', data], { encoding: 'utf8', env });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(report.status, 'installed');
  assert.equal(report.build.command, 'cargo');
  const invocations = readFileSync(calls, 'utf8').trim().split('\n').map(line => JSON.parse(line));
  assert.equal(invocations.length, 1);
  assert.equal(invocations[0].at(-1), path.join(data, 'tools'));
});

test('a binary given with --aether-bin is never rebuilt', t => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'ae-install-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const binary = fakeAether(path.join(dir, 'aether'), { adapter: false });
  const env = { ...process.env, PATH: path.join(dir, 'no-cargo') };
  const result = spawnSync(process.execPath, [installer, '--data-dir', path.join(dir, 'data'), '--aether-bin', binary], { encoding: 'utf8', env });
  assert.equal(result.status, 1);
  assert.match(JSON.parse(result.stdout).error, /lacks connected-adapter support/);
});
