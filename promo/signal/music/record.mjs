// Plays music/score.strudel.js in headless Chromium in real time and writes
// the captured mix to music/raw.wav. `trim.py` then aligns it to the film.
import { chromium } from "playwright-core";
import { createServer } from "node:http";
import { readFile, writeFile } from "node:fs/promises";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const SECONDS = Number(process.env.REC_SECONDS ?? 84);
const executablePath = process.env.CHROMIUM ?? `${process.env.HOME}/Library/Caches/ms-playwright/chromium-1243/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing`;

const server = createServer(async (req, res) => {
  try {
    const url = req.url.split("?")[0];
    const body = await readFile(join(here, url === "/" ? "record.html" : url.slice(1)));
    res.writeHead(200, { "content-type": url.endsWith(".js") ? "text/javascript" : "text/html" });
    res.end(body);
  } catch {
    console.log("[server 404]", req.url);
    res.writeHead(404).end();
  }
}).listen(0);
const port = server.address().port;

const browser = await chromium.launch({ executablePath, headless: true, args: ["--autoplay-policy=no-user-gesture-required"] });
const page = await browser.newPage();
const t0 = Date.now();
page.on("console", (m) => {
  const text = m.text();
  if (text.includes("too late")) console.log(`[scheduler] skipped a window at ${((Date.now() - t0) / 1000).toFixed(1)} s wall time`);
  else if (process.env.VERBOSE || m.type() === "error") console.log(`[page ${m.type()}]`, text);
});
page.on("pageerror", (e) => console.log("[page error]", e.message));
page.on("response", (r) => r.status() >= 400 && console.log("[http]", r.status(), r.url()));
await page.goto(`http://127.0.0.1:${port}/?score=${process.env.SCORE ?? "score.strudel.js"}`);
console.log(await page.evaluate(() => window.__start()), `at ${((Date.now() - t0) / 1000).toFixed(1)} s wall time`);
for (let s = 0; s < SECONDS; s += 10) {
  await page.waitForTimeout(10_000);
  process.stdout.write(`${s + 10}s `);
}
const { rate, b64 } = await page.evaluate(() => window.__wav());
await browser.close();
server.close();

const pcm = Buffer.from(b64, "base64");
const header = Buffer.alloc(44);
header.write("RIFF", 0);
header.writeUInt32LE(36 + pcm.length, 4);
header.write("WAVEfmt ", 8);
header.writeUInt32LE(16, 16);
header.writeUInt16LE(1, 20);
header.writeUInt16LE(2, 22);
header.writeUInt32LE(rate, 24);
header.writeUInt32LE(rate * 4, 28);
header.writeUInt16LE(4, 32);
header.writeUInt16LE(16, 34);
header.write("data", 36);
header.writeUInt32LE(pcm.length, 40);
await writeFile(join(here, "raw.wav"), Buffer.concat([header, pcm]));
console.log(`\nwrote music/raw.wav · ${(pcm.length / 4 / rate).toFixed(1)} s at ${rate} Hz`);
