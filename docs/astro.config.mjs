import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { execFile } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const repo = 'https://github.com/flowcopilot/aether';
const sourceDir = fileURLToPath(new URL('.', import.meta.url));
const agentGuide = fileURLToPath(new URL('../server/priv/static/docs.md', import.meta.url));

export default defineConfig({
  site: 'https://flowcopilot.github.io',
  base: '/aether',
  trailingSlash: 'always',
  outDir: '../target/docs-site',
  integrations: [{
    name: 'aether-markdown-reload',
    hooks: {
      'astro:server:setup': ({ server, logger }) => {
        server.watcher.add([sourceDir, agentGuide]);
        let timer;
        server.watcher.on('all', (event, path) => {
          if (!['add', 'change', 'unlink'].includes(event)) return;
          if (path !== agentGuide && !/^([^/]+|onboarding\/[^/]+)\.md$/.test(path.slice(sourceDir.length))) return;
          clearTimeout(timer);
          timer = setTimeout(() => {
            execFile(process.execPath, [fileURLToPath(new URL('./scripts/prepare-content.mjs', import.meta.url))], (error) => {
              if (error) logger.error(error.message);
            });
          }, 100);
        });
        server.httpServer?.once('close', () => clearTimeout(timer));
      },
    },
  }, starlight({
    title: 'Aether',
    description: 'Share your personal AI agent with people you trust.',
    favicon: '/favicon.svg',
    social: [{ icon: 'github', label: 'GitHub', href: repo }],
    customCss: [
      '@fontsource-variable/bricolage-grotesque/standard.css',
      '@fontsource/ibm-plex-mono/400.css',
      '@fontsource/ibm-plex-mono/600.css',
      './src/styles/docs.css',
    ],
    sidebar: [
      { label: 'Start here', items: [
        { label: 'Introduction', slug: '' },
        { label: 'For humans', slug: 'onboarding/human' },
        { label: 'For agents', slug: 'onboarding/agent' },
        { label: 'Installation', slug: 'installation' },
      ] },
      { label: 'Guides', items: [
        { label: 'Using Aether', slug: 'usage' },
        { label: 'Configuration', slug: 'configuration' },
        { label: 'The PACT gateway', slug: 'pact' },
        { label: 'Development', slug: 'development' },
      ] },
    ],
  })],
});
