// Adapt the canonical Markdown guides without maintaining a second copy.
import { mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('../', import.meta.url)));
const generated = resolve(root, 'src/content/docs');
const base = '/aether/';

async function guides(dir = '') {
  const entries = await readdir(resolve(root, dir), { withFileTypes: true });
  return entries.flatMap((entry) => {
    const path = dir ? `${dir}/${entry.name}` : entry.name;
    if (entry.isDirectory() && entry.name === 'onboarding') return [guides(path)];
    return entry.isFile() && entry.name.endsWith('.md') ? [path] : [];
  });
}

await mkdir(generated, { recursive: true });
const files = (await Promise.all(await guides())).flat();
for (const file of await readdir(generated, { recursive: true })) {
  if (file.endsWith('.md') && !files.includes(file)) await rm(resolve(generated, file));
}
async function writeIfChanged(path, content) {
  const previous = await readFile(path, 'utf8').catch((error) => {
    if (error.code !== 'ENOENT') throw error;
    return null;
  });
  if (previous !== content) await writeFile(path, content);
}
for (const file of files) {
  let content = await readFile(resolve(root, file), 'utf8');
  content = content.replace('<!-- aether:agent-guide -->', await readFile(resolve(root, '../server/priv/static/docs.md'), 'utf8'));
  const heading = content.match(/^# (.+)$/m);
  if (!heading) throw new Error(`Missing title: ${file}`);
  const title = heading[1];
  content = content.replace(heading[0], '').trim();
  // Rewrite local Markdown links outside fenced examples. Raw agent.md stays untouched.
  let inFence = false;
  content = content.split('\n').map((line) => {
    if (/^\s*```/.test(line)) inFence = !inFence;
    if (inFence) return line;
    return line.replace(/(\]\(|href=")([^\s"()]+?\.(?:md|html))(?=[#)"])/g, (match, prefix, target) => {
      if (/^(?:[a-z]+:|\/)/i.test(target)) return match;
      const path = resolve(dirname(resolve(root, file)), target);
      if (!path.startsWith(`${root}/`)) throw new Error(`Link escapes docs: ${file}: ${target}`);
      const route = path.slice(root.length + 1).replace(/\.(md|html)$/, '').replace(/^index$/, '');
      return `${prefix}${base}${route}${route ? '/' : ''}`;
    });
  }).join('\n');
  const output = resolve(generated, file);
  await mkdir(dirname(output), { recursive: true });
  const source = file === 'onboarding/agent.md' ? 'server/priv/static/docs.md' : `docs/${file}`;
  const editUrl = `https://github.com/flowcopilot/aether/edit/main/${source}`;
  await writeIfChanged(output, `---\ntitle: ${JSON.stringify(title)}\neditUrl: ${JSON.stringify(editUrl)}\n---\n\n${content}\n`);
}
await mkdir(resolve(root, 'public'), { recursive: true });
await writeIfChanged(resolve(root, 'public/agent.md'), await readFile(resolve(root, '../server/priv/static/docs.md'), 'utf8'));
console.log(`Prepared ${files.length} guides and raw agent.md`);
