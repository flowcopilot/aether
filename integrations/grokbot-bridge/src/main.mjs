import { readFileSync, statSync, mkdirSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { Mailbox } from './mailbox.mjs';
import { NodeLink, contract } from './node-link.mjs';
import { createBridgeServer, createMcpServer, localExecutor } from './server.mjs';
import { UNAVAILABLE, acquireRuntime } from './runtime.mjs';

function secret(file) {
  const stat = statSync(file);
  if (!stat.isFile() || (stat.mode & 0o077) !== 0) throw new Error('Secret file must be owner-only (chmod 600)');
  const value = readFileSync(file, 'utf8').trim();
  if (value.length < 32 || /\s/.test(value)) throw new Error('Secret must contain at least 32 characters without whitespace');
  return value;
}

// Keeps the last non-empty line a child wrote to stderr, so its exit can be
// reported with its own reason.
function lastLine() {
  let partial = '';
  let last = '';
  return {
    push(chunk) {
      const lines = (partial + chunk.toString('utf8')).split('\n');
      partial = lines.pop().slice(-4096);
      const line = lines.map(value => value.trim()).filter(Boolean).at(-1);
      if (line) last = line;
    },
    get value() { return (partial.trim() || last).slice(0, 500); },
  };
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

  const controller = new AbortController();
  const { signal } = controller;
  // The tools call whichever executor this process has: its own mailbox, or
  // the bridge that owns the node. Before either exists they ask for a retry.
  let executor;
  const current = () => {
    if (!executor) throw new Error(UNAVAILABLE);
    return executor;
  };
  const tools = {
    status: () => current().status(),
    claim: claimId => current().claim(claimId),
    progress: (id, lease) => current().progress(id, lease),
    finish: (id, lease, outcome, text) => current().finish(id, lease, outcome, text),
  };
  let server;
  let child;
  let runtime;
  let proxy;
  let timer;
  let clientOpen = true;
  let proxies = 0;
  const stop = () => {
    if (signal.aborted) return;
    controller.abort();
    clearInterval(timer);
    if (transport === 'http') { server?.close(); server?.closeAllConnections(); }
    else { void server?.close(); process.stdin.pause(); }
    proxy?.close();
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
      const kill = setTimeout(() => child.kill('SIGKILL'), 5000);
      kill.unref();
      child.once('exit', () => clearTimeout(kill));
    } else {
      runtime?.release();
    }
  };
  // The owner keeps the node while any connector still uses it.
  const idle = () => { if (!clientOpen && proxies === 0) stop(); };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);

  const openMailbox = () => {
    let link;
    const mailbox = new Mailbox({ marker: path.join(stateDir, 'active-claim.json'), send: message => link.send(message), wake });
    link = new NodeLink(path.join(dataDir, contract.socket_file), message => mailbox.receive(message), status => mailbox.setConnected(status));
    if (mailbox.active) console.error('Prior claim requires stop acknowledgement; no new work can be claimed');
    timer = setInterval(() => {
      mailbox.tick();
      if (mailbox.connected && mailbox.pending.size && !mailbox.active) void wake();
    }, 1000);
    void link.run(signal);
    return localExecutor(mailbox);
  };

  const startNode = () => {
    const stderr = lastLine();
    child = spawn(process.env.AETHER_BIN || 'aether', ['node', 'serve', '--adapter', 'connected', '--data-dir', dataDir,
      '--policy', path.join(dataDir, 'policy.json')], { stdio: ['ignore', 2, 'pipe'] });
    child.stderr.on('data', chunk => { process.stderr.write(chunk); stderr.push(chunk); });
    if (child.pid) runtime.recordNode(child.pid);
    let ended = false;
    const end = message => {
      if (ended) return;
      ended = true;
      runtime.release();
      if (!signal.aborted) { process.exitCode = 1; console.error(message); stop(); }
    };
    child.once('error', () => end('Could not start the local Aether binary'));
    // `close` follows the last stderr chunk, so the reason is complete.
    child.once('close', (code, exitSignal) => {
      const status = exitSignal ? `signal ${exitSignal}` : `code ${code}`;
      const reason = stderr.value;
      end(`Local Aether node exited (${status})${reason ? `: ${reason}` : ''}; closing the executor`);
    });
  };

  // Own the node, or forward to the bridge that does. When that bridge goes
  // away, try again: this process may become the owner.
  const attach = async () => {
    const claim = await acquireRuntime({ stateDir, dataDir, signal });
    if (signal.aborted) { if (claim.role === 'owner') claim.release(); else claim.executor.close(); return; }
    if (claim.role === 'proxy') {
      proxy = claim.executor;
      executor = proxy;
      console.error('Another bridge owns the local Aether node; forwarding executor tools to it');
      void proxy.closed.then(() => {
        if (executor === proxy) executor = undefined;
        proxy = undefined;
        if (!signal.aborted) void attachOrStop();
      });
      return;
    }
    runtime = claim;
    executor = openMailbox();
    await runtime.serve(executor, count => { proxies = count; idle(); });
    startNode();
  };
  const attachOrStop = () => attach().catch(error => {
    if (signal.aborted) return;
    process.exitCode = 1;
    console.error(error.code ? `Bridge startup failed (${error.code})` : error.message);
    stop();
  });

  if (transport === 'stdio') {
    server = createMcpServer(tools);
    await server.connect(new StdioServerTransport());
    const closed = () => { clientOpen = false; idle(); };
    process.stdin.once('end', closed);
    process.stdin.once('close', closed);
  } else {
    server = createBridgeServer({ executor: tools, token, publicUrl });
    server.listen(port, '127.0.0.1');
    await once(server, 'listening');
  }
  if (process.env.GROKBOT_START_NODE === '1') {
    if (!signal.aborted) await attachOrStop();
  } else {
    executor = openMailbox();
  }
  if (!signal.aborted) console.error(`Experimental Grok Bot executor ready (${transport}); remote stop is cooperative`);
  if (!signal.aborted) await once(signal, 'abort');
}

main().catch(error => { console.error(error.code ? `Bridge startup failed (${error.code})` : error.message); process.exitCode = 1; });
