import { readFileSync, statSync, mkdirSync, rmdirSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { Mailbox } from './mailbox.mjs';
import { NodeLink, contract } from './node-link.mjs';
import { createBridgeServer, createMcpServer } from './server.mjs';

function secret(file) {
  const stat = statSync(file);
  if (!stat.isFile() || (stat.mode & 0o077) !== 0) throw new Error('Secret file must be owner-only (chmod 600)');
  const value = readFileSync(file, 'utf8').trim();
  if (value.length < 32 || /\s/.test(value)) throw new Error('Secret must contain at least 32 characters without whitespace');
  return value;
}

async function main() {
  if (process.env.GROKBOT_EXPERIMENTAL !== '1') throw new Error('Set GROKBOT_EXPERIMENTAL=1 to acknowledge that hosted Bot stopping is cooperative, not guaranteed');
  const transport = process.env.GROKBOT_TRANSPORT || 'http';
  if (!['http', 'stdio'].includes(transport)) throw new Error('GROKBOT_TRANSPORT must be http or stdio');
  const token = transport === 'http' ? secret(process.env.GROKBOT_TOKEN_FILE) : undefined;
  const dataDir = process.env.AETHER_DATA_DIR?.trim() || path.join(os.homedir(), '.aether', 'host');
  // Separate state from node authority. Do not delete the node data directory.
  const stateDir = path.join(dataDir, 'grokbot-bridge');
  mkdirSync(stateDir, { recursive: true, mode: 0o700 });
  const port = Number(process.env.GROKBOT_PORT || 8788);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Invalid GROKBOT_PORT');
  const publicUrl = process.env.GROKBOT_PUBLIC_URL;
  if (publicUrl && new URL(publicUrl).protocol !== 'https:') throw new Error('GROKBOT_PUBLIC_URL requires HTTPS');
  const wakeUrl = process.env.GROKBOT_WAKE_URL;
  if (wakeUrl && new URL(wakeUrl).protocol !== 'https:') throw new Error('GROKBOT_WAKE_URL requires HTTPS');
  const wakeToken = wakeUrl ? secret(process.env.GROKBOT_WAKE_TOKEN_FILE) : undefined;
  let waking = false;
  let lastWake = 0;
  const wake = async () => {
    if (!wakeUrl || waking || Date.now() - lastWake < 30_000) return;
    waking = true;
    lastWake = Date.now();
    try {
      const response = await fetch(wakeUrl, { method: 'POST', redirect: 'error',
        headers: { Authorization: `Bearer ${wakeToken}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ type: 'aether.work_available' }), signal: AbortSignal.timeout(5000) });
      await response.body?.cancel();
      if (response.status !== 200) console.error('Grok routine wake was not accepted; tasks remain queued until their deadlines');
    } catch { console.error('Grok routine wake failed; tasks remain queued until their deadlines'); }
    finally { waking = false; }
  };
  let link;
  const mailbox = new Mailbox({ marker: path.join(stateDir, 'active-claim.json'), send: message => link.send(message), wake });
  link = new NodeLink(path.join(dataDir, contract.socket_file), message => mailbox.receive(message), status => mailbox.setConnected(status));
  const controller = new AbortController();
  let server;
  let child;
  const stop = () => {
    if (controller.signal.aborted) return;
    controller.abort();
    if (transport === 'http') { server?.close(); server?.closeAllConnections(); }
    else { void server?.close(); process.stdin.pause(); }
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
      const kill = setTimeout(() => child.kill('SIGKILL'), 5000);
      kill.unref();
      child.once('exit', () => clearTimeout(kill));
    }
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  if (transport === 'stdio') {
    server = createMcpServer(mailbox);
    await server.connect(new StdioServerTransport());
    process.stdin.once('end', stop);
    process.stdin.once('close', stop);
  } else {
    server = createBridgeServer({ mailbox, token, publicUrl });
    server.listen(port, '127.0.0.1');
    await once(server, 'listening');
  }
  if (process.env.GROKBOT_START_NODE === '1' && !controller.signal.aborted) {
    const lock = path.join(stateDir, 'runtime.lock');
    try { mkdirSync(lock, { mode: 0o700 }); }
    catch { stop(); throw new Error('Node runtime is locked; stop any prior bridge and node before removing grokbot-bridge/runtime.lock'); }
    child = spawn(process.env.AETHER_BIN || 'aether', ['node', 'serve', '--adapter', 'connected', '--data-dir', dataDir,
      '--policy', path.join(dataDir, 'policy.json')], { stdio: ['ignore', 2, 2] });
    const release = () => { rmdirSync(lock); };
    child.once('error', () => { release(); process.exitCode = 1; console.error('Could not start the local Aether binary'); stop(); });
    child.once('exit', () => {
      release();
      if (!controller.signal.aborted) { process.exitCode = 1; console.error('Local Aether node exited; closing the executor'); stop(); }
    });
  }
  console.error(`Experimental Grok Bot executor ready (${transport}); remote stop is cooperative`);
  if (mailbox.active) console.error('Prior claim requires stop acknowledgement; no new work can be claimed');
  const timer = setInterval(() => {
    mailbox.tick();
    if (mailbox.connected && mailbox.pending.size && !mailbox.active) void wake();
  }, 1000);
  try { await link.run(controller.signal); }
  finally { clearInterval(timer); stop(); }
}

main().catch(error => { console.error(error.code ? `Bridge startup failed (${error.code})` : error.message); process.exitCode = 1; });
