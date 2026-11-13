# Agent socket protocol

An agent that runs on its own (the Hermes and OpenClaw plugins) connects to
the node's agent socket and answers the tasks that the node sends it. The
node side is `crates/aether/src/agent/connected.rs`. The agent sides are
`integrations/hermes-plugin/node.py` and
`integrations/openclaw-plugin/src/{protocol,node-socket,task-runner}.ts`.

`contract.json` holds the constants and examples. `transcript.json` holds
the exchanges that every implementation replays in its tests. The plugins
are installed without this folder, so they keep their own copies of the
constants, and their tests compare the copies with `contract.json`.

## Rules

- The socket is `<data dir>/<socket_file>`, mode `0600`: only the node
  owner's processes can attach. The default data directory is
  `$AETHER_DATA_DIR` (trimmed) when it is not blank, else
  `$HOME/.aether/host`.
- Each message is one JSON object on one line that ends with `\n`. A line
  has at most `max_line_bytes` bytes, the `\n` included. A longer line closes
  the connection.
- A line that is blank, not a JSON object, of an unknown `type`, or without
  the fields of its type is skipped. The connection stays open.
- The agent says `ready` with `protocol` first. The node closes a
  connection whose first message is not `ready` with its protocol. When a
  second agent says `ready`, the node closes the first connection, and the
  tasks of the first connection fail.
- `ready` may carry `max_tasks`, a whole number: how many open tasks the
  agent takes at once. Without it the agent takes `default_max_tasks` (one).
  The node starts a task only while fewer tasks run than the smaller of
  `max_tasks` and the owner's running-task limit. It reads `max_tasks` from
  the first `ready` of a connection. An agent that says `max_tasks` of 2
  or more runs those tasks side by side: each task is answered, cancelled
  and stopped by its own ID, and an answer or a cancel for one task never
  touches another.
- The node sends `task` and `cancel`. The agent sends `progress`, `result`,
  `failed` and `cancelled`. The node sends only the task text and its
  instructions: no keys, grants, endpoint IDs or caller names. The agent
  sends only the answer text.
- Each task gets one final answer: `result`, `failed` or `cancelled`. The
  node ignores an answer for a task that is not open.
- The node never sends a second `task` with the ID of an open task. The agent
  ignores one.
- On `cancel` of an open task, the agent stops the turn and sends
  `cancelled` at once. It drops the turn's later answers. The node sends
  `cancel` when the caller cancels or when the turn's time limit ends.
- When the connection closes, the node fails its open tasks, and the agent
  stops their turns and forgets them. The agent connects again every
  `reconnect_seconds` seconds.
- The agent gives its model the task in a frame: the first line of the
  `frame.examples`, the trimmed instructions, the `--- task text ---` line,
  then the text unchanged. The frame starts with `[`, so the task text can
  never start a slash command.

## Transcript steps

Each case is a list of steps, in order, and the final `outcomes` of its
tasks. A test replays the case with its own side under test and a fake peer.

- `{"agent": message}`, `{"node": message}`: a message from that side. The
  side under test must send it; the fake peer writes it.
- `"ignored": true` on a step: the receiver takes no action.
  - On an agent message, the agent's turn produces this answer after the
    task is closed. An agent under test does not send it. A node under test
    gets it and does not change the task.
  - On a node `task`, an agent under test starts no second turn. A node
    under test refuses to send it: the turn fails at once.
- `"concurrent": true` on a case: it has more than one open task at once.
  An agent that takes one task at a time (the Grok Bot bridge) skips it.
  The `ready` that an agent under test sends carries its own `max_tasks`, so
  a replay compares only the `type` and `protocol` of a `ready`.
- `{"agent_line": text}`, `{"node_line": text}`: a line that the fake peer
  writes. The side under test never sends one, so its replay skips the step.
- `{"agent_line_bytes": n}`, `{"node_line_bytes": n}`: as above, a line of
  exactly `n` bytes with the `\n`: `{"type":"noise","pad":"x…x"}`.
- `{"closes": "node"}`, `{"closes": "agent"}`: that side closes the
  connection. The next `ready` is a new connection.
- `outcomes`: for each task ID, `answered`, `failed`, `cancelled` or `lost`
  (the connection closed first). For the agent side, `cancelled` and `lost`
  mean that the turn was stopped.
- The node fills `instructions` from the task's skill. A node replay
  compares them with its own instructions for the general skill.
