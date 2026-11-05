// Plugin entry. It registers the "aether" channel and a before_tool_call guard.
// The guard limits the tools that an Aether task may call (src/tool-guard.ts).
import { defineChannelPluginEntry } from "openclaw/plugin-sdk/channel-core";
import { aetherChannelPlugin, allowedToolsFrom } from "./src/channel.ts";
import { CHANNEL_ID, DESCRIPTION } from "./src/inbound.ts";
import { decideToolCall } from "./src/tool-guard.ts";

export default defineChannelPluginEntry({
  id: CHANNEL_ID,
  name: "Aether",
  description: DESCRIPTION,
  plugin: aetherChannelPlugin,
  registerFull(api) {
    api.on("before_tool_call", (event, ctx) => {
      const decision = decideToolCall({
        sessionKey: ctx.sessionKey,
        channel: ctx.requester?.channel,
        toolName: event.toolName,
        allowedTools: allowedToolsFrom(api.config),
      });
      return decision.block ? { block: true, blockReason: decision.reason } : undefined;
    });
  },
});
