// OpenClaw reads package.json and openclaw.plugin.json before it loads the
// plugin code, so these files repeat names from the code.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { CHANNEL_ID, DESCRIPTION } from "../src/inbound.ts";

const read = (name: string) => JSON.parse(readFileSync(new URL(`../${name}`, import.meta.url), "utf8"));
const pkg = read("package.json");
const manifest = read("openclaw.plugin.json");

test("the manifests name the channel and describe it as the code does", () => {
  assert.equal(manifest.id, CHANNEL_ID);
  assert.deepEqual(manifest.channels, [CHANNEL_ID]);
  assert.equal(pkg.openclaw.channel.id, CHANNEL_ID);
  assert.equal(manifest.description, DESCRIPTION);
  assert.equal(pkg.openclaw.channel.blurb, DESCRIPTION);
});

test("the MCP server of the plugin serves only the caller tools", () => {
  assert.ok(manifest.mcpServers.aether.args.includes("--caller-only"));
});
