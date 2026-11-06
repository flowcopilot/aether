// The "aether" channel. Its gateway.startAccount keeps one connection to the
// node socket and turns each node task into an OpenClaw turn (src/inbound.ts).
import os from "node:os";
import path from "node:path";
import { createChatChannelPlugin } from "openclaw/plugin-sdk/channel-core";
import type { OpenClawConfig } from "openclaw/plugin-sdk/channel-core";
import { PlatformMessageNotDispatchedError } from "openclaw/plugin-sdk/error-runtime";
import { channelReadyPatch, channelStoppedPatch } from "openclaw/plugin-sdk/gateway-runtime";
import { resolveAgentRoute } from "openclaw/plugin-sdk/routing";
import type { PluginRuntime } from "openclaw/plugin-sdk/runtime-store";
import { CHANNEL_ID, DESCRIPTION, createDispatch } from "./inbound.ts";
import { NodeSocket } from "./node-socket.ts";
import { defaultDataDir, socketPath } from "./protocol.ts";
import { TaskRunner } from "./task-runner.ts";

const DEFAULT_ACCOUNT_ID = "default";

export interface AetherAccount {
  accountId: string;
  enabled: boolean;
  configured: boolean;
  dataDir: string;
  allowedTools: string[];
}

export function channelSection(cfg: OpenClawConfig): Record<string, unknown> | undefined {
  const channels: unknown = (cfg as { channels?: unknown }).channels;
  if (!isRecord(channels) || !isRecord(channels[CHANNEL_ID])) {
    return undefined;
  }
  return channels[CHANNEL_ID];
}

export function allowedToolsFrom(cfg: OpenClawConfig): string[] {
  const value = channelSection(cfg)?.allowedTools;
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

// A "~/" prefix means the home directory, as the README shows. Without a
// setting, the same default as the aether command is used.
export function resolveDataDir(configured: unknown): string {
  if (typeof configured !== "string" || configured.trim() === "") {
    return defaultDataDir();
  }
  const value = configured.trim();
  return value.startsWith("~/") ? path.join(os.homedir(), value.slice(2)) : value;
}

function resolveAccount(cfg: OpenClawConfig, accountId?: string | null): AetherAccount {
  const section = channelSection(cfg);
  return {
    accountId: accountId ?? DEFAULT_ACCOUNT_ID,
    enabled: section?.enabled !== false,
    configured: section !== undefined,
    dataDir: resolveDataDir(section?.dataDir),
    allowedTools: allowedToolsFrom(cfg),
  };
}

// The owner's Aether agent is named by a binding with match.channel "aether".
// These warnings read the config and do not change it.
function bindingAgentIds(cfg: OpenClawConfig): string[] {
  const bindings: unknown = (cfg as { bindings?: unknown }).bindings;
  if (!Array.isArray(bindings)) {
    return [];
  }
  const ids: string[] = [];
  for (const binding of bindings) {
    if (!isRecord(binding) || !isRecord(binding.match) || binding.match.channel !== CHANNEL_ID) {
      continue;
    }
    if (typeof binding.agentId === "string") {
      ids.push(binding.agentId);
    }
  }
  return ids;
}

function sandboxMode(cfg: OpenClawConfig, agentId: string): unknown {
  const agents: unknown = (cfg as { agents?: unknown }).agents;
  if (!isRecord(agents) || !isRecord(agents.entries)) {
    return undefined;
  }
  const entry = agents.entries[agentId];
  return isRecord(entry) && isRecord(entry.sandbox) ? entry.sandbox.mode : undefined;
}

export const aetherChannelPlugin = createChatChannelPlugin<AetherAccount>({
  base: {
    id: CHANNEL_ID,
    meta: {
      id: CHANNEL_ID,
      label: "Aether",
      selectionLabel: "Aether (agent network)",
      // No OpenClaw docs page exists for this channel yet. Point this at the
      // plugin README before publishing.
      docsPath: "/channels/aether",
      docsLabel: "aether",
      blurb: DESCRIPTION,
      order: 80,
    },
    capabilities: { chatTypes: ["direct"] },
    reload: { configPrefixes: ["channels.aether"] },
    config: {
      listAccountIds: (cfg) => (channelSection(cfg) ? [DEFAULT_ACCOUNT_ID] : []),
      resolveAccount,
      defaultAccountId: () => DEFAULT_ACCOUNT_ID,
      isConfigured: (account) => account.configured,
      isEnabled: (account) => account.enabled,
    },
    gateway: {
      startAccount: async (ctx) => {
        // The gateway passes its runtime to channels. Without it there is no
        // way to admit or dispatch a task, so startup fails.
        const channelRuntime = ctx.channelRuntime as PluginRuntime["channel"] | undefined;
        if (channelRuntime === undefined) {
          throw new Error("The Aether channel needs the OpenClaw channel runtime.");
        }
        const { account } = ctx;
        const dispatch = createDispatch({
          cfg: ctx.cfg,
          accountId: account.accountId,
          channelRuntime,
          resolveRoute: resolveAgentRoute,
        });

        const socket = new NodeSocket({
          path: socketPath(account.dataDir),
          onMessage: (message) => runner.handle(message),
          onStatus: (connected) => {
            if (!connected) {
              runner.close();
            }
            ctx.setStatus({
              accountId: account.accountId,
              running: true,
              connected,
              configured: true,
              enabled: account.enabled,
            });
          },
        });
        const runner = new TaskRunner({
          send: (message) => {
            socket.send(message);
          },
          dispatch,
        });

        ctx.setStatus(channelReadyPatch({ accountId: account.accountId }));
        try {
          // Resolves only after the gateway aborts this account. Until then the
          // socket reconnects every few seconds.
          await socket.run(ctx.abortSignal);
        } finally {
          ctx.setStatus(channelStoppedPatch({ accountId: account.accountId }));
        }
      },
    },
  },
  security: {
    collectWarnings: ({ cfg }) => {
      const warnings: string[] = [];
      const agentIds = bindingAgentIds(cfg);
      for (const agentId of agentIds) {
        const mode = sandboxMode(cfg, agentId);
        if (mode === undefined || mode === "off") {
          warnings.push(
            `The Aether channel runs tasks in agent "${agentId}" without a sandbox. Set agents.entries.${agentId}.sandbox.mode to "all".`,
          );
        }
      }
      const tools: unknown = (cfg as { tools?: unknown }).tools;
      const sessions = isRecord(tools) && isRecord(tools.sessions) ? tools.sessions : undefined;
      if (agentIds.length > 0 && sessions?.visibility === "all") {
        warnings.push(
          'tools.sessions.visibility is "all". Aether task agents can then read every session. Set it to "self".',
        );
      }
      return warnings;
    },
  },
  outbound: {
    base: { deliveryMode: "direct" },
    attachedResults: {
      channel: CHANNEL_ID,
      // Aether has no reply after a task ends. The agent sends a new task with
      // the caller tools instead.
      sendText: async () => {
        throw new PlatformMessageNotDispatchedError(
          "Aether has no reply after a task ends. Send a new Aether task instead.",
          { cause: undefined, retryable: false },
        );
      },
    },
  },
});

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
