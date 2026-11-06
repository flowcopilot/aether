// The task lifecycle of protocol/agent-socket: maps one node task to one
// OpenClaw dispatch. OpenClaw stays behind the injected Dispatch, so this file
// has no OpenClaw import and can be tested.
import { frameTask } from "./protocol.ts";
import type { AgentMessage, NodeMessage } from "./protocol.ts";

export type DeliverKind = "tool" | "block" | "final";
export type Dispatch = (
  task: { id: string; body: string; signal: AbortSignal },
  hooks: {
    deliver: (text: string, kind: DeliverKind) => void;
    onTerminal: (outcome: "completed" | "failed") => void;
  },
) => Promise<void>;

interface OpenTask {
  controller: AbortController;
  finish: (message: AgentMessage) => void;
}

export class TaskRunner {
  readonly #send: (message: AgentMessage) => void;
  readonly #dispatch: Dispatch;
  readonly #open = new Map<string, OpenTask>();

  constructor(options: { send: (message: AgentMessage) => void; dispatch: Dispatch }) {
    this.#send = options.send;
    this.#dispatch = options.dispatch;
  }

  handle(message: NodeMessage): void {
    if (message.type === "task") {
      this.#start(message.id, frameTask(message.instructions, message.text));
      return;
    }
    const task = this.#open.get(message.id);
    if (task !== undefined) {
      task.finish({ type: "cancelled", id: message.id });
      task.controller.abort();
    }
  }

  close(): void {
    const tasks = [...this.#open.values()];
    this.#open.clear();
    for (const task of tasks) {
      task.controller.abort();
    }
  }

  #start(id: string, body: string): void {
    if (this.#open.has(id)) {
      return;
    }
    const controller = new AbortController();
    // The hooks of this run answer only while this run is the open task, so a
    // late hook never answers a later run with the same id.
    const task: OpenTask = {
      controller,
      finish: (message) => {
        if (this.#open.get(id) === task) {
          this.#open.delete(id);
          this.#send(message);
        }
      },
    };
    this.#open.set(id, task);

    const deliver = (text: string, kind: DeliverKind): void => {
      if (kind === "final") {
        task.finish({ type: "result", id, text });
      } else if (this.#open.get(id) === task) {
        // Progress carries no text, so no OpenClaw detail reaches the node.
        this.#send({ type: "progress", id });
      }
    };
    const onTerminal = (outcome: "completed" | "failed"): void => {
      // A turn that ends without a final text still answers the node.
      task.finish(outcome === "failed" ? { type: "failed", id } : { type: "result", id, text: "" });
    };

    // A dispatch that throws or rejects fails the task. Its error text is
    // never sent. The executor form also catches a synchronous throw.
    new Promise<void>((resolve) =>
      resolve(this.#dispatch({ id, body, signal: controller.signal }, { deliver, onTerminal })),
    ).catch(() => task.finish({ type: "failed", id }));
  }
}
