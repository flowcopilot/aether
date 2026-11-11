import net from 'node:net';
import path from 'node:path';
import { chmodSync, mkdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';
import { publicError } from './server.mjs';

// One bridge owns the local node: it holds `runtime.lock`, runs the node and
// its mailbox, and serves the executor tools on `bridge.sock`. A client can
// open more than one command connector; every later bridge forwards its tool
// calls to the owner instead of starting a second node.

export const UNAVAILABLE = 'Bridge is not connected to its node runtime yet; retry shortly';
const OWNER = 'owner.json';
const MAX_LINE = 256 * 1024;
// An owner writes owner.json right after making the lock. A lock still without
// one after this long was left by a bridge that stopped in between.
const OWNERLESS_MS = 2000;

export function alive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { return error.code === 'EPERM'; }
}

function readOwner(lock) {
  try {
    const owner = JSON.parse(readFileSync(path.join(lock, OWNER), 'utf8'));
    return Number.isInteger(owner?.pid) && owner.pid > 0 ? owner : undefined;
  } catch { return undefined; }
}

function writeOwner(lock, owner) {
  const file = path.join(lock, OWNER);
  // Rename so a reader never sees a half-written file.
  writeFileSync(`${file}.tmp`, JSON.stringify(owner), { mode: 0o600 });
  renameSync(`${file}.tmp`, file);
}

// Whether `pid` is still the node a stopped bridge started. On Linux its
// command line must name `serve` and this data directory, so a reused pid is
// never signalled. Elsewhere a live pid is reported as unknown.
function orphanNode(pid, dataDir) {
  if (!Number.isInteger(pid) || pid <= 0 || !alive(pid)) return false;
  try {
    const args = readFileSync(`/proc/${pid}/cmdline`, 'utf8').split('\0');
    return args.includes('serve') && args.includes(dataDir) ? true : false;
  } catch { return undefined; }
}

function abandoned(lock) {
  try { return Date.now() - statSync(lock).mtimeMs > OWNERLESS_MS; }
  catch { return false; }
}

async function stopOrphanNode(pid, dataDir) {
  const orphan = orphanNode(pid, dataDir);
  if (orphan === false) return;
  if (orphan === undefined) {
    throw new Error(`Aether node process ${pid} from a stopped bridge may still be running; stop it, then restart the connector`);
  }
  const kill = name => { try { process.kill(pid, name); } catch {} };
  kill('SIGTERM');
  for (let waited = 0; alive(pid); waited += 100) {
    if (waited === 5000) kill('SIGKILL');
    if (waited > 10_000) throw new Error(`Aether node process ${pid} from a stopped bridge did not stop; stop it, then restart the connector`);
    await sleep(100);
  }
}

// Removes a lock whose owner process is gone, or that never recorded one
// (`stale` is then empty). The lock is renamed first and
// checked again, so two bridges taking over at once cannot remove the lock
// the faster one has just made.
async function takeOver(lock, stale, dataDir) {
  await stopOrphanNode(stale.node, dataDir);
  const moved = `${lock}.stale-${process.pid}`;
  try { renameSync(lock, moved); } catch { return; }
  if (readOwner(moved)?.pid === stale.pid) { rmSync(moved, { recursive: true, force: true }); return; }
  try { renameSync(moved, lock); } catch {}
}

/**
 * Becomes the node owner or a proxy to the bridge that already is.
 * Owner: `{ role: 'owner', serve, recordNode, release }`.
 * Proxy: `{ role: 'proxy', executor }`; `executor.closed` settles when the owner goes away.
 */
export async function acquireRuntime({ stateDir, dataDir, waitMs = 10_000, signal }) {
  const lock = path.join(stateDir, 'runtime.lock');
  const socketPath = path.join(stateDir, 'bridge.sock');
  const deadline = Date.now() + waitMs;
  for (;;) {
    signal?.throwIfAborted();
    try {
      mkdirSync(lock, { mode: 0o700 });
      writeOwner(lock, { pid: process.pid });
      return ownerLease(lock, socketPath);
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
    }
    const owner = readOwner(lock);
    if (owner ? !alive(owner.pid) : abandoned(lock)) {
      await takeOver(lock, owner ?? {}, dataDir);
      continue;
    }
    if (owner) {
      const executor = await connectExecutor(socketPath).catch(() => undefined);
      if (executor) return { role: 'proxy', executor };
    }
    if (Date.now() >= deadline) {
      throw new Error(owner
        ? `Bridge process ${owner.pid} holds grokbot-bridge/runtime.lock but does not answer; stop it and its Aether node, then restart the connector`
        : 'Node runtime is locked by a bridge that did not record its process; stop any prior bridge and node before removing grokbot-bridge/runtime.lock');
    }
    // The owner may still be starting: it makes the lock before its socket.
    await sleep(250, undefined, signal ? { signal } : undefined);
  }
}

function ownerLease(lock, socketPath) {
  let released = false;
  let server;
  return {
    role: 'owner',
    recordNode(pid) { if (!released) writeOwner(lock, { pid: process.pid, node: pid }); },
    async serve(executor, onClients) {
      server = await serveExecutor(socketPath, executor, onClients);
      return server;
    },
    release() {
      if (released) return;
      released = true;
      server?.close();
      // The socket goes before the lock: a new owner listens only after taking the lock.
      rmSync(socketPath, { force: true });
      rmSync(lock, { recursive: true, force: true });
    },
  };
}

function lines(socket, receive) {
  let buffer = '';
  socket.setEncoding('utf8');
  socket.on('data', chunk => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      let message;
      try { message = JSON.parse(line); } catch { socket.destroy(); return; }
      receive(message);
    }
    if (buffer.length > MAX_LINE) socket.destroy();
  });
  socket.on('error', () => {});
}

const METHODS = {
  status: executor => executor.status(),
  claim: (executor, { claim_id }) => executor.claim(claim_id),
  progress: (executor, { id, lease }) => executor.progress(id, lease),
  finish: (executor, { id, lease, outcome, text }) => executor.finish(id, lease, outcome, text),
};

async function answer(executor, request) {
  const id = request?.id;
  if (!Object.hasOwn(METHODS, request?.method)) return { id, error: 'Unknown bridge request' };
  try { return { id, result: await METHODS[request.method](executor, request.params ?? {}) }; }
  catch (error) { return { id, error: publicError(error) }; }
}

export async function serveExecutor(socketPath, executor, onClients = () => {}) {
  rmSync(socketPath, { force: true });
  const clients = new Set();
  const server = net.createServer(socket => {
    clients.add(socket);
    onClients(clients.size);
    lines(socket, request => {
      void answer(executor, request).then(reply => {
        if (!socket.destroyed) socket.write(JSON.stringify(reply) + '\n');
      });
    });
    socket.on('close', () => { clients.delete(socket); onClients(clients.size); });
  });
  server.listen(socketPath);
  await new Promise((resolve, reject) => {
    server.once('listening', resolve);
    server.once('error', reject);
  });
  chmodSync(socketPath, 0o600);
  const close = server.close.bind(server);
  server.close = callback => {
    for (const socket of clients) socket.destroy();
    return close(callback);
  };
  return server;
}

export function connectExecutor(socketPath) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(socketPath);
    socket.once('error', reject);
    socket.once('connect', () => { socket.off('error', reject); resolve(remoteExecutor(socket)); });
  });
}

function remoteExecutor(socket) {
  const waiting = new Map();
  let next = 0;
  let closed;
  const done = new Promise(resolve => { closed = resolve; });
  lines(socket, reply => {
    const call = waiting.get(reply?.id);
    if (!call) return;
    waiting.delete(reply.id);
    if (typeof reply.error === 'string') call.reject(new Error(reply.error));
    else call.resolve(reply.result);
  });
  socket.on('close', () => {
    for (const call of waiting.values()) call.reject(new Error(UNAVAILABLE));
    waiting.clear();
    closed();
  });
  const call = (method, params) => new Promise((resolve, reject) => {
    if (socket.destroyed) { reject(new Error(UNAVAILABLE)); return; }
    next += 1;
    waiting.set(next, { resolve, reject });
    socket.write(JSON.stringify({ id: next, method, params }) + '\n');
  });
  return {
    status: () => call('status'),
    claim: claim_id => call('claim', { claim_id }),
    progress: (id, lease) => call('progress', { id, lease }),
    finish: (id, lease, outcome, text) => call('finish', { id, lease, outcome, text }),
    closed: done,
    close: () => socket.destroy(),
  };
}
