# Test Alice's agent with Bob's agent

Alice and Bob each run one Aether node and one Codex agent on separate Macs.
Alice asks her agent to send a task to Bob's agent. Bob reads his agent's full
answer on his Aether page. Alice's agent receives the same answer.

The hub at `https://aether-hub.tudorandrei.xyz` finds people by name. The relay
at `https://aether-relay.tudorandrei.xyz` connects the two computers.

## Prepare both computers

1. Get the current code. On each computer, clone the repo, or pull it if it
   is already there:

   ```sh
   git clone https://github.com/flowcopilot/aether.git
   cd aether
   mise install
   mise run install-aether
   aether --version
   ```

2. Start a node on each computer. Keep this terminal open:

   ```sh
   aether node serve
   ```

   The first start creates the policy in `~/.aether/host/node.sqlite` and the
   task folder root at `~/.aether/tasks`. An old `~/.aether/policy.json`, if
   present, is imported once. Each task gets a new empty folder. The node
   removes it when the task ends. The agent cannot read other files on that
   computer or use the network from the task folder.

   Each person needs to be logged in to Codex on their own computer. Aether
   uses that login without loading their personal Codex settings. If the node
   prints a login command, run it once on that computer, then start the node
   again. Keep both Macs awake for the test.

3. Each person opens the local HTTPS URL that their node prints and accepts
   the certificate warning. The node uses
   `https://aether-relay.tudorandrei.xyz` by default. Alice saves the name
   `alice`. Bob saves the name `bob`. The two people must use different names.

## Connect Alice to Bob

4. On Bob's page, under **Who can use your agent**, invite `alice`. Bob's node
   gives Alice access to his agent. New invites join the `friends` access
   group.
5. On Alice's page, open **Agents you can use** and accept Bob's invite. Bob
   appears there with a green dot when his node is online.
6. Bob checks that Alice appears under **Who can use your agent** and in the
   `friends` group.

## Send the task

7. Alice starts a new Codex session in the Aether repo. Run `/mcp` and check
   that `aether` is connected. This repo's MCP setup uses Alice's own
   `~/.aether/host` node data.
8. Alice asks her agent:

   > Ask Bob's agent to write one short greeting for Bob. Send the task with
   > Aether, and show me Bob's agent's answer.

   Alice's agent uses `aether.agents.list` to find Bob and `aether.task.send`
   to send the task. Bob's node starts Bob's Codex agent in a new empty task
   folder.
9. Bob reloads his Aether page. Under Alice's last task, he opens
    **Read answer** to see the task and the full answer. Alice's Codex session
    also shows the answer.

If `aether` is not connected in Alice's Codex session, copy the Codex command
under **Let your own agent send tasks** on Alice's **Agents you can use** page.
Run it once, then start a new Codex session.

To stop Alice's access, Bob selects **Revoke** next to her name. If the hub
cannot be reached, the pages offer connect codes instead of name invites.

## Send to an agent that is offline

10. Bob stops his node with Ctrl-C.
11. Alice asks her agent to send Bob's agent a second task. After about 20 s,
    `aether.task.send` returns `status: "waiting"` and a task ID. Alice's
    page shows the task under **Sent tasks** as **Waiting**.
12. Bob starts his node again with `aether node serve`. Within about one
    minute, Alice's node sees Bob online through the hub and sends the task
    again. Bob's agent does the task once.
13. Alice asks her agent to read the answer with `aether.inbox.read` and the
    task ID. Alice's page shows the task as **Answered**.
14. Repeat steps 10 to 13 with the relay of Alice's node turned off (clear
    **Set a relay** on her page, or start it with `--relay-url off`). The task
    then stays **Waiting**, because Alice's node cannot reach Bob's node. Turn
    the relay on again: the task is sent, and the answer arrives.
