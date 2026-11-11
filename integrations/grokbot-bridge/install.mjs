// Installer output is JSON; build logs go to stderr. No shell interpolation.
import { parseArgs } from 'node:util';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import os from 'node:os';

try {
  const { values } = parseArgs({ options: {
    'dry-run': { type: 'boolean', default: false },
    'data-dir': { type: 'string' },
    'aether-bin': { type: 'string' },
    help: { type: 'boolean', default: false },
  } });
  if (values.help) {
    console.log(JSON.stringify({ usage: 'node install.mjs [--dry-run] [--data-dir PATH] [--aether-bin PATH]',
      purpose: 'Build a local Aether binary if needed and return a command-based MCP configuration',
      prerequisites: ['Node.js >=24', 'Rust >=1.98, a C compiler and CMake when building Aether'],
      effects: ['cargo install into the dedicated data directory if no binary is installed', 'creates an owner-only data directory'],
      start: 'The returned MCP command starts/stops its own node. Register it once with Grok Bot.' }, null, 2));
  } else {
    if (Number(process.versions.node.split('.')[0]) < 24) throw new Error('Node.js 24 or later is required');
    if (!['linux', 'darwin'].includes(process.platform)) throw new Error('Only Linux and macOS are supported');
    const directory = path.dirname(fileURLToPath(import.meta.url));
    const root = path.resolve(directory, '../..');
    const dataDir = path.resolve(values['data-dir'] || path.join(os.homedir(), '.aether', 'grokbot'));
    if (/[\x00-\x1f\x7f]/.test(dataDir)) throw new Error('Data directory contains a control character');
    const tools = path.join(dataDir, 'tools');
    const binary = values['aether-bin'] ? path.resolve(values['aether-bin']) : path.join(tools, 'bin', 'aether');
    if (/[\x00-\x1f\x7f]/.test(binary)) throw new Error('Binary path contains a control character');
    const owned = !values['aether-bin'];
    let build = owned && !existsSync(binary);
    let cargoArgs = ['install', '--path', path.join(root, 'crates/aether'), '--locked', '--root', tools];
    const config = { command: process.execPath, args: [path.join(directory, 'src/main.mjs')], env: {
      AETHER_DATA_DIR: dataDir, AETHER_BIN: binary, GROKBOT_TRANSPORT: 'stdio', GROKBOT_START_NODE: '1', GROKBOT_EXPERIMENTAL: '1',
    } };
    if (!values['dry-run']) {
      if (!existsSync(path.join(directory, 'node_modules/@modelcontextprotocol/sdk/package.json'))) throw new Error('Run npm ci in integrations/grokbot-bridge first');
      mkdirSync(dataDir, { recursive: true, mode: 0o700 });
      const compile = () => {
        console.error('Building Aether locally; Rust 1.98+, a C compiler and CMake are required.');
        const result = spawnSync('cargo', cargoArgs, { cwd: root, stdio: ['ignore', 2, 2] });
        if (result.error || result.status !== 0) throw new Error('Aether build failed; install the documented build prerequisites and retry');
      };
      const supported = () => {
        const probe = spawnSync(binary, ['node', 'serve', '--help'], { encoding: 'utf8' });
        return !probe.error && probe.status === 0 && probe.stdout.includes('--adapter');
      };
      if (build) compile();
      let ready = supported();
      // A binary this installer built earlier may predate connected-adapter
      // support: replace it once. A binary given with --aether-bin is never touched.
      if (!ready && owned && !build) {
        console.error('The installed Aether binary lacks connected-adapter support; rebuilding it.');
        build = true;
        cargoArgs = ['install', '--force', ...cargoArgs.slice(1)];
        compile();
        ready = supported();
      }
      if (!ready) throw new Error('Aether binary is unavailable or lacks connected-adapter support');
    }
    console.log(JSON.stringify({ status: values['dry-run'] ? 'planned' : 'installed', data_dir: dataDir,
      build: build ? { command: 'cargo', args: cargoArgs } : null,
      mcpServers: { 'aether-executor': config },
      next: 'Register this command-based MCP server once, then call bridge_status until connected is true. No public HTTPS URL or bearer token is needed for stdio.' }, null, 2));
  }
} catch (error) {
  console.log(JSON.stringify({ status: 'error', error: error.message }));
  process.exitCode = 1;
}
