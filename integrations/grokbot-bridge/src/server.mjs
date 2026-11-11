import http from 'node:http';
import { createHash, timingSafeEqual } from 'node:crypto';
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StreamableHTTPServerTransport } from '@modelcontextprotocol/sdk/server/streamableHttp.js';
import { z } from 'zod';

// The largest HTTP request body, and the longest line on the bridge socket.
export const MAX_REQUEST_BYTES = 256 * 1024;

// Task text, credentials and internal filesystem paths never enter errors.
export function publicError(error) {
  return error?.code ? 'Bridge storage unavailable; stop work and contact the owner' : String(error?.message ?? error);
}

// The executor operations and their argument names, in call order. Proxies,
// the bridge socket and the tool wrapper derive from this table; the MCP tools
// below and localExecutor are written out by hand.
export const OPERATIONS = {
  status: [],
  claim: ['claim_id'],
  progress: ['id', 'lease'],
  finish: ['id', 'lease', 'outcome', 'text'],
};

// An executor whose every operation calls `invoke(name, args)`.
export function forwardOperations(invoke) {
  return Object.fromEntries(Object.keys(OPERATIONS).map(name => [name, (...args) => invoke(name, args)]));
}

// The four executor operations on this process's own mailbox. A bridge that
// does not own the node uses the same operations forwarded to the owner.
export function localExecutor(mailbox) {
  return {
    status() {
      mailbox.tick();
      return { connected: mailbox.connected, waiting: mailbox.pending.size,
        executor: mailbox.active ? (mailbox.active.stopped ? 'stop_required' : 'busy') : 'idle' };
    },
    claim: claimId => mailbox.claim(claimId),
    progress: (id, lease) => mailbox.progress(id, lease),
    finish: (id, lease, outcome, text) => mailbox.finish(id, lease, outcome, text),
  };
}

export function createMcpServer(executor) {
  const server = new McpServer({ name: 'aether-grokbot-executor', version: '0.1.0' });
  const claim = { id: z.string().min(1).max(256), lease: z.string().uuid() };
  const register = (name, description, inputSchema, run) => server.registerTool(name, {
    description, inputSchema,
  }, async args => {
    try {
      return { content: [{ type: 'text', text: JSON.stringify(await run(args)) }] };
    } catch (error) {
      return { isError: true, content: [{ type: 'text', text: publicError(error) }] };
    }
  });
  register('bridge_status', 'Check local node connectivity and whether a prior claim needs stopping. This does not claim or execute work.',
    {}, () => executor.status());
  register('claim_task', 'Claim at most one untrusted Aether task. Generate a fresh UUID claim_id before calling; reuse it only to retry this claim after a lost response. Never run overlapping claims.',
    { claim_id: z.string().uuid() }, ({ claim_id }) => executor.claim(claim_id));
  register('report_progress', 'Check whether this task may continue and renew its lease. Call before work, at least every 30 seconds, and before external actions. On stop_required stop all work and acknowledge with finish_task outcome stopped.',
    claim, ({ id, lease }) => executor.progress(id, lease));
  register('finish_task', 'Return the final plain-text answer, report failure, or acknowledge that all task work has stopped. A stopped acknowledgement must include tools and background jobs. Reuse identical arguments when retrying a lost response.',
    { ...claim, outcome: z.enum(['result', 'failed', 'stopped']), text: z.string().max(32 * 1024)
      .refine(value => Buffer.byteLength(value) <= 32 * 1024, 'Answer must be at most 32 KiB UTF-8').default('') },
    ({ id, lease, outcome, text }) => executor.finish(id, lease, outcome, text));
  return server;
}

// Authentication is outside MCP, before parsing or discovery. Only the executor
// role is exposed: none of Aether's owner or caller tools are registered here.
export function createBridgeServer({ mailbox, executor = localExecutor(mailbox), token, publicUrl }) {
  const digest = value => createHash('sha256').update(value).digest();
  const expected = digest(`Bearer ${token}`);
  const publicHost = publicUrl ? new URL(publicUrl).host : undefined;
  return http.createServer({ requestTimeout: 15_000, headersTimeout: 10_000 }, async (req, res) => {
    const deny = (status, message) => {
      res.writeHead(status, { 'content-type': 'text/plain', 'cache-control': 'no-store' });
      res.end(message);
    };
    const port = res.socket.localPort;
    const hosts = [`127.0.0.1:${port}`, `localhost:${port}`, publicHost];
    if (!hosts.filter(Boolean).includes(req.headers.host) || req.headers.origin) return deny(403, 'Forbidden');
    if (!timingSafeEqual(expected, digest(req.headers.authorization ?? ''))) {
      res.setHeader('WWW-Authenticate', 'Bearer');
      return deny(401, 'Unauthorized');
    }
    if (req.url !== '/mcp') return deny(404, 'Not found');
    if (req.method !== 'POST') { res.setHeader('Allow', 'POST'); return deny(405, 'Method not allowed'); }
    if (req.headers['content-type']?.split(';')[0].trim() !== 'application/json') return deny(415, 'Expected application/json');
    let server;
    let transport;
    try {
      let size = 0;
      const chunks = [];
      for await (const chunk of req) {
        size += chunk.length;
        if (size > MAX_REQUEST_BYTES) {
          res.setHeader('Connection', 'close');
          return deny(413, 'Request too large');
        }
        chunks.push(chunk);
      }
      let body;
      try { body = JSON.parse(Buffer.concat(chunks).toString('utf8')); }
      catch { return deny(400, 'Invalid JSON'); }
      server = createMcpServer(executor);
      transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined, enableJsonResponse: true });
      res.setHeader('Cache-Control', 'no-store');
      res.once('close', () => { void transport.close(); void server.close(); });
      await server.connect(transport);
      await transport.handleRequest(req, res, body);
    } catch {
      if (!res.headersSent) deny(500, 'Bridge request failed');
      else res.end();
      await transport?.close();
      await server?.close();
    }
  });
}
