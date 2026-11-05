// Maps one node task to one OpenClaw dispatch. OpenClaw stays behind the
// injected Dispatch, so this file has no OpenClaw import and can be tested.
// The rules are the Decisions in research/openclaw-integration.md.
import { frameTask } from "./protocol.ts";
import type { AgentMessage, NodeMessage } from "./protocol.ts";

export type DeliverKind = "tool" | "block" | "final";
export type Dispatch = (
  task: { id: string; body: string },
  hooks: {
    deliver: (text: string, kind: DeliverKind) => void;
    onTerminal: (outcome: "completed" | "failed") => void;
  },
) => Promise<void>;

export class TaskRunner {
  readonly #send: (message: AgentMessage) => void;
  readonly #dispatch: Dispatch;
  // Open tasks, each with its finish function. A task leaves the map when it
  // finishes, so the map is the list of tasks that still need an answer.
  readonly #open = new Map<string, (message: AgentMessage) => void>();

  constructor(options: { send: (message: AgentMessage) => void; dispatch: Dispatch }) {
    this.#send = options.send;
    this.#dispatch = options.dispatch;
  }

  get activeCount(): number {
    return this.#open.size;
  }

  handle(message: NodeMessage): void {
    if (message.type === "task") {
      this.#start(message.id, frameTask(message.instructions, message.text));
    } else {
      this.#open.get(message.id)?.({ type: "cancelled", id: message.id });
    }
  }

  #start(id: string, body: string): void {
    // A duplicate of a running task is ignored.
    if (this.#open.has(id)) {
      return;
    }
    // Each run has its own done flag. Hooks of a finished run then cannot
    // answer a later run that reuses the same id.
    let done = false;
    const finish = (message: AgentMessage): void => {
      if (done) {
        return;
      }
      done = true;
      this.#open.delete(id);
      this.#send(message);
    };
    this.#open.set(id, finish);

    const deliver = (text: string, kind: DeliverKind): void => {
      if (kind === "final") {
        finish({ type: "result", id, text });
      } else {
        // Progress carries no text, so no OpenClaw detail reaches the node.
        if (!done) {
          this.#send({ type: "progress", id });
        }
      }
    };
    const onTerminal = (outcome: "completed" | "failed"): void => {
      if (outcome === "failed") {
        finish({ type: "failed", id });
      } else {
        // A turn that ends without a final text still answers the node.
        finish({ type: "result", id, text: "" });
      }
    };

    // A dispatch that throws or rejects fails the task. Its error text is
    // never sent. The executor form also catches a synchronous throw.
    new Promise<void>((resolve) => resolve(this.#dispatch({ id, body }, { deliver, onTerminal }))).catch(
      () => finish({ type: "failed", id }),
    );
  }
}
