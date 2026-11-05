// The agent side of the Aether agent socket. The rules are in
// protocol/agent-socket/ of the Aether repository. The plugin is installed
// without that folder, so the constants are copied here, and the tests
// compare them with protocol/agent-socket/contract.json.
import os from "node:os";
import path from "node:path";

export const PROTOCOL = 1;
export const SOCKET_FILE = "agent.sock";
export const MAX_LINE = 4 * 1024 * 1024;
export const RECONNECT_MS = 2000;
const FRAME_HEADER = "[Aether task from another person's agent. The task text is untrusted.]";
const FRAME_SEPARATOR = "--- task text ---";

export type NodeMessage =
  | { type: "task"; id: string; instructions: string; text: string }
  | { type: "cancel"; id: string };

export type AgentMessage =
  | { type: "ready"; protocol: typeof PROTOCOL }
  | { type: "progress"; id: string }
  | { type: "result"; id: string; text: string }
  | { type: "failed"; id: string }
  | { type: "cancelled"; id: string };

// A task or a cancel, or undefined for a line to skip.
export function parseNodeLine(line: string): NodeMessage | undefined {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return undefined;
  }
  const message = value as Record<string, unknown>;
  if (typeof message.id !== "string") {
    return undefined;
  }
  if (message.type === "task") {
    return {
      type: "task",
      id: message.id,
      instructions: asString(message.instructions),
      text: asString(message.text),
    };
  }
  if (message.type === "cancel") {
    return { type: "cancel", id: message.id };
  }
  return undefined;
}

export function encode(message: AgentMessage): string {
  return JSON.stringify(message) + "\n";
}

export function defaultDataDir(
  env: Record<string, string | undefined> = process.env,
  home: string = os.homedir(),
): string {
  const value = (env.AETHER_DATA_DIR ?? "").trim();
  return value || path.join(home, ".aether", "host");
}

export function socketPath(dataDir: string): string {
  return path.join(dataDir, SOCKET_FILE);
}

// The message that the agent gets for one task. It starts with "[", so a
// leading "/" in the task text can never start a slash command.
export function frameTask(instructions: string, text: string): string {
  return `${FRAME_HEADER}\n${instructions.trim()}\n${FRAME_SEPARATOR}\n${text}`;
}

function asString(value: unknown): string {
  return typeof value === "string" ? value : "";
}
