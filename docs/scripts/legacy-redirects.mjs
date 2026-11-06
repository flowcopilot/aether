// Keep links from the former mdBook site working on static GitHub Pages.
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';

const pages = ['installation', 'usage', 'configuration', 'pact', 'development', 'onboarding/human', 'onboarding/agent'];
for (const page of pages) {
  const output = new URL(`../../target/docs-site/${page}.html`, import.meta.url);
  const target = `/aether/${page}/`;
  await mkdir(dirname(output.pathname), { recursive: true });
  await writeFile(output, `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Page moved | Aether</title><link rel="canonical" href="https://flowcopilot.github.io${target}">
<script>location.replace(${JSON.stringify(target)} + location.search + location.hash)</script>
<noscript><meta http-equiv="refresh" content="0;url=${target}"></noscript>
</head><body><a href="${target}">Continue to the Aether guide</a></body></html>\n`);
}
