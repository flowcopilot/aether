# Install Aether for an existing Grok Bot

Use this guide when the owner asks you, Grok Bot, to join Aether. Install on
your persistent cloud computer. This path uses command-based MCP over stdio;
it needs neither a public domain nor a bearer token. The connector owns one
local Aether node for this Bot. The implementation is experimental and stopping
remote work remains cooperative.

## 1. Prepare the computer

Check for Git, Node.js 24+, npm, Rust 1.98+, a C compiler, and CMake. Install
missing prerequisites using the computer's supported tools and the official
[Node](https://nodejs.org/en/download) and [Rust](https://www.rust-lang.org/tools/install)
installation guidance. Keep installation within the user's account where possible.
If policy or the environment prevents a prerequisite, report the exact blocker.

The first setup builds Aether from source; it can take several minutes and
needs network access for Rust dependencies and the build's Tailwind download.
This installer uses the source checkout rather than depending on a published
binary release.

```sh
mkdir -p "$HOME/.local/share"
git clone --depth 1 --branch main https://github.com/flowcopilot/aether.git "$HOME/.local/share/aether-source"
cd "$HOME/.local/share/aether-source/integrations/grokbot-bridge"
npm ci
node install.mjs --dry-run
node install.mjs
```

If the clone directory exists, inspect and reuse the installation rather than
overwriting it. Preserve local changes. The installer returns JSON containing
the exact MCP command, arguments and environment. Build output goes to stderr.
Success is `status: installed`; a failed command is not a completed install.
To use a suitable binary already installed elsewhere, pass its absolute path
with `--aether-bin /absolute/path/to/aether` and skip the source-build prerequisites.

## 2. Register the command connector

Use Grok Bot's supported custom MCP installation capability to register the
returned `mcpServers.aether-executor` entry **once**. Use its `command`, `args`
and `env` exactly as returned. This is a command server running on your computer,
not an HTTP URL. Do not start a second copy in a terminal. Aether node logs go to
stderr so stdout stays valid MCP. The MCP process starts its own Aether node and
stops that child when the connector closes.

Call `bridge_status` until `connected: true` (allow a few seconds). Normally it
also reports `executor: idle`. Call `claim_task` with a new UUID `claim_id`; an
empty queue should return `status: empty`. If `executor: stop_required` or a
claim is `blocked`, follow the [recovery procedure](README.md#cancellation-recovery-and-shutdown)
before starting new work. Do not report completion merely because the connector
is listed. Installation is verified only after these real tool calls succeed.

The MCP command's startup lock is `<data-dir>/grokbot-bridge/runtime.lock`.
The first connector process takes it and owns the Aether node. If Grok opens
the connector again while that process runs, the new process forwards its
tool calls to the owner over `grokbot-bridge/bridge.sock` instead of starting
a second node. The owner keeps the node while any connector still uses it;
when the owner exits, a remaining connector takes over and starts the node again.
Normal shutdown removes the lock. A lock left by a crashed bridge is taken over
automatically. On Linux the new bridge first stops the node the crashed bridge
left running. Elsewhere it reports that node's process ID for you to stop.
Remove the lock by hand only when the error asks you to, after verifying the
old bridge and its Aether child are stopped. The separate active-claim marker
has stricter recovery requirements; keep it until old task work is stopped.

If the bridge exits with `Local Aether node exited`, the text after the colon
is the node's own last error line. A deleted default task folder
(`<data-dir>/../tasks`) is made again at startup. A missing custom
`workspace` in the policy is reported with its path.

## 3. Connect the owner and receive tasks

The installer reports `data_dir` and the binary path in the connector environment.
Use that binary with `--data-dir` pointing at this directory for local node CLI
operations. Discover its commands with `aether node --help` and
`aether node admin --help`; use the actual installed binary path in place of
`aether`. Follow [Aether onboarding](https://flowcopilot.github.io/aether/agent.md) to connect to
the owner's other node or invited contacts. Creating this new node alone does
not connect it to the owner's existing node or grant access to contacts.

Save the [worker instructions](README.md#teach-the-bot-to-execute-one-task) as a
skill, including claim retry IDs, untrusted-task handling, progress checks and
stop acknowledgement. Start with one harmless text-only task while the owner
is present. Verify that the other Aether node receives the final answer.

For unattended reception, create a webhook routine using those worker
instructions and configure the optional wake URL/key described in
[Wake a Bot](README.md#wake-a-bot-when-work-arrives). Add `GROKBOT_WAKE_URL` and
`GROKBOT_WAKE_TOKEN_FILE` to the command connector's environment and restart
the connector when no task is running. If Grok keeps the webhook key inaccessible
to you, have the owner provide it through a secure credential path. This step
may require the owner; never substitute a key pasted into ordinary chat.
Routine polling at five-minute intervals is slower than the task deadline.

Report separately: installation verified, owner/peer connected, first answer
verified, and unattended reception verified. Claim only stages actually observed.
Grok's command registration, process lifetime and routine behavior still need
live qualification on the owner's account. This guide describes the supported
implementation path, not a completed live Grok installation.

All Bots in one Grok account share their computer and command connectors.
Use one owning Bot for this bridge; Bot names do not isolate credentials or files.
