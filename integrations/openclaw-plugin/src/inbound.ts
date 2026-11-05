// Turns one Aether task into one OpenClaw channel turn. This follows the a2a
// inbound path (extensions/a2a/src/inbound.ts in OpenClaw) and the Decisions in
// research/openclaw-integration.md.
//
// The file has only type imports from OpenClaw, so the tests load it without
// the openclaw package. The route resolver is passed in for the same reason.
import type { OpenClawConfig } from "openclaw/plugin-sdk/config-contracts";
import type { resolveAgentRoute } from "openclaw/plugin-sdk/routing";
import type { PluginRuntime } from "openclaw/plugin-sdk/runtime-store";
import type { Dispatch } from "./task-runner.ts";

export const CHANNEL_ID = "aether";

// The node checks the grant before it sends a task. The plugin admits every
// task as this one fixed sender, so the sender is never a caller name. It is
// never the owner.
export const NODE_SENDER_ID = "aether-node";

export interface DispatchOptions {
  cfg: OpenClawConfig;
  accountId: string;
  channelRuntime: PluginRuntime["channel"];
  resolveRoute: typeof resolveAgentRoute;
}

export function createDispatch(options: DispatchOptions): Dispatch {
  const { cfg, accountId, channelRuntime, resolveRoute } = options;

  return async (task, hooks) => {
    // Each task gets its own session. The node sends no caller identity, so the
    // task id is the only safe key. A task never joins the owner's main session.
    const route = resolveRoute({
      cfg,
      channel: CHANNEL_ID,
      accountId,
      peer: { kind: "direct", id: task.id },
      dmScope: "per-account-channel-peer",
    });

    const ingress = await channelRuntime.inbound.ingress.resolveStable({
      channelId: CHANNEL_ID,
      accountId,
      cfg,
      identity: { key: "sender", entryIdPrefix: "aether-entry" },
      subject: { stableId: NODE_SENDER_ID },
      conversation: { kind: "direct", id: task.id },
      contextBinding: {
        agentId: route.agentId,
        sessionKey: route.sessionKey,
        messageId: task.id,
        inboundEventKind: "user_request",
      },
      dmPolicy: "allowlist",
      allowFrom: [NODE_SENDER_ID],
    });
    if (ingress.ingress.admission !== "dispatch") {
      hooks.onTerminal("failed");
      return;
    }

    // The body already starts with "[", so OpenClaw never reads it as a slash
    // command. CommandInterpretationSuppressed is a second guard for the same case.
    const target = `${CHANNEL_ID}:${task.id}`;
    const ctxPayload = channelRuntime.inbound.buildContext({
      channel: CHANNEL_ID,
      accountId: route.accountId ?? accountId,
      messageId: task.id,
      messageIdFull: task.id,
      timestamp: Date.now(),
      from: target,
      sender: { id: NODE_SENDER_ID, name: "Aether node" },
      conversation: { kind: "direct", id: task.id, label: "Aether task" },
      route: {
        agentId: route.agentId,
        dmScope: route.dmScope,
        accountId: route.accountId,
        routeSessionKey: route.sessionKey,
        dispatchSessionKey: route.sessionKey,
      },
      reply: { to: target, originatingTo: target },
      message: {
        body: task.body,
        bodyForAgent: task.body,
        rawBody: task.body,
        commandBody: task.body,
      },
      channelIngress: ingress,
      extra: { CommandInterpretationSuppressed: true },
    });

    const result = await channelRuntime.inbound.dispatch({
      cfg,
      channel: CHANNEL_ID,
      accountId,
      route: { agentId: route.agentId, dmScope: route.dmScope, sessionKey: route.sessionKey },
      ctxPayload,
      delivery: {
        deliver: async (payload, info) => {
          // Only the text of a final reply goes to the node. Tool and block
          // deliveries carry no text, so no progress detail leaves the plugin.
          hooks.deliver(payload.text ?? "", info.kind);
        },
        onError: () => {
          // The error text stays here. The node gets only "failed".
          hooks.onTerminal("failed");
        },
      },
      replyOptions: {
        sourceReplyDeliveryMode: "automatic",
        // "completed" is not used here. It arrives before the final reply is
        // delivered, so it would send an empty result. The dispatch result below
        // ends a completed task after its reply is delivered.
        onAgentRunTerminalOutcome: (outcome) => {
          if (outcome === "failed") {
            hooks.onTerminal("failed");
          }
        },
      },
      replyPipeline: {},
    });

    if (result.admission.kind !== "dispatch" || !result.dispatched) {
      hooks.onTerminal("failed");
      return;
    }
    // A queued turn is answered later through a follow-up path this plugin does
    // not handle. A new session per task makes this rare. Failing is safer than
    // waiting for a reply that never comes.
    if (result.dispatchResult.deferredToActiveRun) {
      hooks.onTerminal("failed");
      return;
    }
    hooks.onTerminal("completed");
  };
}
