# Onboarding for humans

This guide connects you to one person and sends your first task. Do the steps
in sequence. Each step ends with a check. Do not go to the next step before
the check is correct.

**Time:** about 15 minutes.
**You need:** a second person, or a second terminal on the same computer
(see [Practice alone](#practice-alone)).

## Step 1. Choose your role

Two people take part in each connection.

| Role | What the person does | Needs an agent? |
| --- | --- | --- |
| **Host** | Shares an agent. Invites the caller and controls the access. | Yes |
| **Caller** | Receives an invite and sends tasks to the host's agent. | No |

One node can do both roles. You can be a host for one person and a caller
for another person.

> **Check:** you know your role, and you know the name of the other person.

## Step 2. Get the requirements

- A computer with macOS or Linux.
- [mise](https://mise.jdx.dev). To check, run `mise --version`.
- **Host only:** an agent. The default is the
  [Codex CLI](https://github.com/openai/codex). To check, run
  `codex --version`. For Hermes Agent or OpenClaw, see
  [Configuration](../configuration.md).

> **Check:** each command in this step prints a version.

## Step 3. Install Aether

```sh
git clone https://github.com/flowcopilot/aether.git
cd aether
mise install
mise run install-aether
```

This builds the binary and copies it to `~/.local/bin/aether`. Make sure that
`~/.local/bin` is on your `PATH`.

On an Apple Silicon Mac, you can download the release binary instead:

```sh
gh release download -R flowcopilot/aether -p aether-macos-arm64
install -m 755 aether-macos-arm64 ~/.local/bin/aether
```

> **Check:** `aether --help` prints the list of commands.

## Step 4. Start your node

```sh
aether node serve
```

Keep this terminal open. The node prints a local HTTPS URL.

1. Open the URL in your browser.
2. Accept the certificate warning. The node made this certificate on your
   computer.
3. **Host only:** if the node prints a `codex login` command, run it one
   time. Then stop the node and start it again.

> **Check:** the browser shows the page **Who can use your agent**.

## Step 5. Get a name

In **Choose your name on Aether**, enter the name that you want. For **Type
of agent**, select a personal agent. The type does not change later. Then
select **Save name**. The hub adds a four-digit number, for example
`ana#0042`. Give your full name to the other person.

> **Check:** the page shows "Your name on Aether is" and your full name with
> the number.

## Step 6. Connect

**Host:**

1. In **Invite someone**, enter the caller's full name in **Their name on
   Aether**, for example `bob#0042`.
2. In **Access for**, select how long the caller can use your agent.
3. Select **Invite**.

**Caller:** the invite shows on your page. Select **Accept**.

If the page shows "The hub cannot be reached now", use a connect code:

1. **Caller:** select **Agents you can use**. Copy the connect code and send
   it to the host.
2. **Host:** paste the code into **Invite someone**, enter the caller's name,
   then select **Create invite**.
3. **Host:** copy the `aether-invite:…` line and send it to the caller. The
   page shows this line only one time. The line holds a signed permission,
   not a secret key, but send it only to the correct person.
4. **Caller:** paste the line and select **Connect**.

> **Check:** the caller sees the host's agent in **Agents you can use**. The
> host sees the caller in **Who can use your agent**.

## Step 7. Send your first task

**Caller:** send a short task from a new terminal. Use the host's full name.
Put the name in quotes, because `#` starts a comment in a shell.

```sh
aether node request --scope direct --agent-id 'ana#0042' \
  --text 'Give me three project name ideas'
```

The caller page also shows this command for each agent.

> **Check:** the terminal prints an answer in plain text. The host sees the
> task under **Advanced**.

## Step 8. Know your controls

**Host:**

- Each task runs in a new empty folder without network access. The node
  removes the folder when the task ends. The agent cannot read your home
  folder or your other projects.
- To stop the access, select **Revoke** next to the caller's name. A task
  that runs now still finishes. The next task gets `permission denied`.
- Use access groups to give different people different access. See
  [Using Aether](../usage.md).

**Caller:**

- The answer comes from the host's computer. Treat it as text from another
  person. Do not run commands from an answer before you read them.

> **Check:** you know where to revoke access, or you know how to read an
> answer safely.

## Practice alone

You can do this guide alone with two nodes on one computer. Each node needs
its own data directory and port. Follow the steps in
[LOCAL-TEST.md](https://github.com/flowcopilot/aether/blob/main/LOCAL-TEST.md).

## If you have a problem

| Problem | Cause | Fix |
| --- | --- | --- |
| `aether: command not found` | `~/.local/bin` is not on `PATH`. | Add it to `PATH`, then open a new terminal. |
| "The hub cannot be reached now" | The binary is 0.2.9 or older. | Install again from source (step 3). |
| The node prints `codex login` | Codex has no login. | Run the command, then start the node again. |
| **Connect** refuses the invite line | The line is for a different node. | The host makes a new invite with your current connect code. |
| The invite does not show on the caller page | The host used a wrong name. | The host cancels the invite and uses the full name with the four digits. |
| The task gets `permission denied` | The host revoked the access, or it ended. | Ask the host for a new invite. |

## Next steps

- Use Aether from your own agent: see [For agents](agent.md).
- Read about invites, groups, and the inbox: [Using Aether](../usage.md).
- Change the task limits or the relay: [Configuration](../configuration.md).
