// Limits the tools that an Aether task may call. A remote task must not reach
// the owner's other tools. This is the second guard, after the dedicated
// agent and its tool profile (research/openclaw-integration.md, Security).

export interface ToolCallInput {
  sessionKey?: string;
  channel?: string;
  toolName: string;
  allowedTools: readonly string[];
}

export type ToolDecision = { block: false } | { block: true; reason: string };

export function decideToolCall(input: ToolCallInput): ToolDecision {
  // The session key holds the channel name between colons, so either test
  // finds an Aether session.
  const inAether = input.channel === "aether" || (input.sessionKey ?? "").includes(":aether:");
  // Names match exactly. A glob would let a new tool slip past the list.
  if (!inAether || input.allowedTools.includes(input.toolName)) {
    return { block: false };
  }
  return { block: true, reason: "This tool is not allowed for Aether tasks." };
}
