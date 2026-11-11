import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

// Polls `check` (sync or async) until it returns a truthy value, and returns it.
export async function until(check, message = 'timed out waiting for the expected state', ms = 7000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    assert.ok(Date.now() < deadline, message);
    await sleep(10);
  }
}

// Writes an executable stand-in for `aether node serve`. It appends its pid to
// `starts`, listens on the agent socket in AETHER_DATA_DIR, answers `ready`
// with one task (`taskId`, default `task-<pid>`), and writes the last result
// it receives to `results`. With `adapter: false` its --help lacks --adapter,
// like a binary built before connected-adapter support.
export function fakeAether(file, { starts, results, taskId, adapter = true }) {
  const options = { starts, results, taskId, adapter };
  writeFileSync(file, `#!${process.execPath}
import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
const options = ${JSON.stringify(options)};
if (process.argv.includes('--help')) { console.log(options.adapter ? '--adapter' : '--data-dir'); process.exit(0); }
if (options.starts) fs.appendFileSync(options.starts, process.pid + '\\n');
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
      if (message.type === 'ready') peer.write(JSON.stringify({ type: 'task', id: options.taskId ?? 'task-' + process.pid, instructions: 'Be brief.', text: 'Name a colour.' }) + '\\n');
      if (message.type === 'result' && options.results) fs.writeFileSync(options.results, JSON.stringify(message));
    }
  });
}).listen(socket);
process.on('SIGTERM', () => process.exit(0));
`, { mode: 0o700 });
  return file;
}

// Starts the installer's command-based MCP server and connects to it. `call`
// returns the parsed tool result, or `{ error }` for a tool error. The caller
// closes `client`.
export async function connectStdio(config) {
  const transport = new StdioClientTransport({ ...config, stderr: 'pipe' });
  transport.stderr?.resume();
  const client = new Client({ name: 'command-host', version: '1.0.0' });
  await client.connect(transport);
  const call = async (name, args = {}) => {
    const response = await client.callTool({ name, arguments: args });
    return response.isError ? { error: response.content[0].text } : JSON.parse(response.content[0].text);
  };
  return { client, call };
}
