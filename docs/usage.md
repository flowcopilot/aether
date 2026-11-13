# Using Aether

## Set up with the web UI

Two people can connect in the direct scope without files or signing commands.
The host runs the agent. The caller sends tasks to it.

Run `aether node serve`. On the first start, it creates the task folder root
at `~/.aether/tasks` and saves a direct-scope policy in
`~/.aether/host/node.sqlite`. It creates `<data-dir>/issuer.key` and trusts it
as the issuer `node`. It does not need grants or trusted issuers in the policy.
If `~/.aether/policy.json` already exists, the first start imports it into the
database. `--policy` selects another file for this one-time import. After that,
the database is the policy source and changes to the old file have no effect.

On the **Advanced** page, **Task policy** sets the task folder root, context
and time limits, queue limits, and how many tasks run at once. The folder must exist. Restart the node
after you save a policy change. **Permissions** links to the invite and access
group controls on the Sharing page.

The default policy has these values:

```json
{
  "scope": { "id": "direct", "kind": "direct" },
  "workspace": "/path/to/task-folders",
  "maxContextTokens": 20000,
  "maxSeconds": 120,
  "queueGlobal": 1,
  "queuePerPeer": 1,
  "runningTasks": 1
}
```

Each task runs in its own new folder under `workspace`. The task can read and
write that folder, but it cannot read the host's home folder or other projects.
The node removes the task folder when the task ends. The task has no network
access. The agent's MCP servers are not in this sandbox (see [Agent tools](configuration.md#agent-tools-mcp-servers)).
The host page does not offer a folder change in this mode.

Set `"sandbox": "workspace-write"` only if you want the previous project
mode. In that mode, Codex can read other files that the host account can read,
and the host page can change the workspace. `allowNetwork` can only be true in
that mode.

```sh
aether node serve
```

A new node uses `https://aether-relay.tudorandrei.xyz` by default. On the host
page, **Set a relay** saves another relay URL for the next start. The running
node moves to that relay at once. The page refuses a relay that does not answer
within a few seconds. Clear the URL to use only the local network. That choice
also stays in effect after a restart. `--relay-url` (or `AETHER_RELAY_URL`)
wins over the saved choice at start. Set either one to `off` for local access
only.

The caller starts the UI without a node. `node admin`, `node direct` and
`node request` use the same data directory when `--data-dir` is not given:
`$AETHER_DATA_DIR`, else `~/.aether/host`.

```sh
aether node admin --port 0
```

`node serve` gives one person both roles. Its main page shows **Who can use
your agent**. Select **Agents you can use** to connect to another person's
agent and see your contacts. Both pages use the same endpoint key and data
directory. The MCP command on the agents page uses that directory too, so one
MCP server can send tasks and administer the running node. `node admin` still
opens a caller page when this computer does not host an agent. **Advanced**
has the counts, task cancellation, activity and changes that wait for
confirmation.

The host page has **Incoming access groups**. New invites join `friends`.
Create groups such as `work`, add or remove people, and delete groups you no
longer need. A caller needs both an active direct grant and membership in at
least one group. Removing a caller from all groups stops new tasks and rejects
tasks that have not started. Group membership uses the caller's name and
endpoint ID. The default `friends` group stays available for new invites.
The same node data directory serves MCP tools for these groups, task policy,
relay and hub URLs, your hub name, invites, and direct access. Use
`aether.settings.get` to read the saved policy and URLs. Use
`aether.settings.update_policy` to edit task limits (`queue_global`, `queue_per_peer`,
`running_tasks`) or the task folder root.
A new task folder root or a higher limit waits until you confirm it under
Pending changes on the Advanced page. Policy changes need a node restart.
`aether.settings.set_relay` changes a running node at once, and
`aether.settings.set_hub` takes effect on the next hub request. The `aether.name.*`, `aether.invites.*`, `aether.access.list`
and `aether.access.revoke` tools cover the matching actions on the Sharing
page. On a node without an agent type, a first `aether.name.set` makes a
personal agent: claim a service agent on the page. `aether.access.request`
only prepares a request (see [Access requests](#access-requests)).
Connect-code invites and direct-access revocation require a running node.

## Connect two people

1. The caller copies the connect code (step 1) and sends it to the host. The
   code is the caller's endpoint ID, in groups of four.
2. The host pastes it into **Invite someone**, enters the caller's name,
   selects how long the access lasts, and selects **Create invite**. The node
   applies the grant at once. The invite names the host by its name on the
   hub. Without a hub name, it uses `--invite-name`, else `agent-` and the
   first 8 characters of the endpoint ID. `--agent-id` names only the
   directory record. The grant has the skill `general`: the agent does the
   task that the caller sends and answers in plain text. Its instruction also
   tells it not to name the tools, MCP servers, model, files, folders or
   settings of the host computer. An older grant with the skill `review`
   runs as `general` in the direct scope.
3. The host copies one of two forms, shown only once, and sends it to the
   caller. Both hold a signed invitation and authorization, not a secret key.
   They also hold the host's Agent Card, signed with the invitation key
   (`aether-invitation/v2`). A 0.2.9 or older caller cannot read a `v2` line, so
   update both nodes:
   - the invite line, `aether-invite:…`, for the caller's page;
   - the command for the caller's terminal:

     ```sh
     aether node direct import 'aether-invite:…'
     ```

4. The caller pastes the invite line into step 2 and selects **Connect**, or
   runs the command. Both refuse an invite for another endpoint ID.
5. The caller's own agent sends tasks, or the command that the caller page
   shows for each agent:

   ```sh
   aether node request --scope direct --agent-id tudor --text 'Give me three project name ideas'
   ```

   The invite gives the authorization and the relay, so the command needs no
   `--authorization`, `--address` or `--relay-url`.

## Use Aether from Codex in this repository

Install the release binary with mise:

```sh
mise run install-aether
```

This repository's `.codex/config.toml` starts the installed Aether MCP server
and uses `~/.aether/host` for the node data. It applies only when Codex works
in this repository. Keep the node running in a terminal:

```sh
aether node serve
```

The node prints a local HTTPS admin URL. Open it to invite people, accept
invitations, and manage incoming groups. Restart Codex in this repository,
then use `/mcp` to check the Aether tools. Ask Codex to list agents you can
use, send a task, edit the saved policy, set the relay, or manage invites and
incoming access. The node must stay running for task and live access tools.
An imported invitation must belong to the endpoint key in `~/.aether/host`.

## Sent tasks and the inbox

- `aether.task.send` keeps the task in the outbox of this node and waits up to
  20 s. It returns the answer (`status: "answered"`, the text in
  `untrusted.answer`), an error (`failed` or `not_delivered`, with a reason),
  or `status: "waiting"` with the task ID.
- `aether node serve` runs the outbox. It sends a waiting task again with the
  same task ID and idempotency key, so the other agent does the task once. It
  tries a full queue again after a back-off, and an agent that the hub shows
  offline every 30 minutes. It reads the presence of its contacts from the hub
  every 60 s while tasks wait.
- `aether.inbox.list` lists the sent tasks, newest first, without the answer
  text. `aether.inbox.read` reads one answer by its task ID, in parts of
  32 KiB.
- A task that its agent does not answer ends as `not_delivered` after 7 days,
  or when the access to that agent ends.
- The other node keeps an answer for 24 hours after the task ends. This node
  must read it in that time.
- When no node runs, `aether.task.send` makes one attempt with a short-lived
  endpoint, then the task waits on this computer for the next start.
- The hub carries no task text and no answer.

## Your agent's card

The host page has **Your agent's card**: a description, one skill name and
skill description, and up to 5 tags. People that you invite get this A2A
Agent Card with the invite; it is never public. The node writes the rest of
the card: the name, the Iroh interface, the security scheme, and plain text
in and out. Do not name your tools, model, folders or settings. Each save
raises the card version.

## Agent types

At its first name on the hub, a node chooses the type of its agent. The type
does not change later.

- A **personal agent** stands for one person. Friends that the person
  invites send it tasks, and it answers at once. Make only one personal agent
  for each person; the hub cannot know a person, so it does not check this.
  It can turn on name search: when it is off, people need its full name.
- A **service agent** offers a service, for example legal advice. A human
  registers it by hand on the page of that node, and needs no personal
  agent. It has no invite-by-name form. People find it in the listings and
  ask for access, and the owner approves each request on its page.

A page without a host has only a personal agent.

## Public listing of a service agent

The page of a service agent has **Public listing**. **List this agent** puts
the text of the agent's card on the hub for 30 days: the full name, the
description and the skill. The listing has no endpoint and no address. The
node signs it with its endpoint key, and the page signs it again when less
than 7 days remain. **Stop listing** removes it. A rename removes it too:
list the agent again after a rename or a change of the card text.

- People find a listed agent by search on the hub. When it is listed,
  people can see when it is online.
- When another agent connects, each computer can see the public IP address
  of the other.
- People who get access by request share the task queue with your friends:
  the node keeps `queueGlobal` waiting tasks in all and `queuePerPeer` for
  each person. Change them in **Advanced**.
  They wait for a free place to run: the node runs `runningTasks` tasks at the
  same time, one by default (see [Tasks that run at once](configuration.md#tasks-that-run-at-once)).

## Access requests

A person who finds a listed service agent can ask it for access. The
request has no free text: only the full name of the person and the days.

An agent only prepares a request. The caller page shows it under **Requests
your agent wants to send**, with the listing text of the service agent
("The agent's owner wrote this. Aether did not check it.") and the IP note.
The person selects **Confirm** to send it, or **Discard**. A prepared
request waits 24 hours, and at most 5 wait at one time. The line that comes
back is kept only when the key that sent it signed the listing.
The page of the service agent shows each request under **Access requests**,
with the key of the person in groups of four characters.

- **Approve** has only the days. The node signs the access for the key that
  the page showed, and for no other key. An approved person always joins the
  access group `requests`, never `friends`. The groups `friends` and
  `requests` cannot be deleted.
- **Decline** removes the request.
- **Block** removes the request and hides later requests from that key. The
  node also keeps the block, so it never signs for that key. The **Blocked**
  list has **Unblock**.

The service agent does not see whether the person is online. The person
sees whether the service agent is online.

## Owner links

A person can link a service agent to their own personal agent, so that its
public listing shows who runs it. The page of the personal agent has **Your
service agents**: type the full name of the service agent and select
**Claim**. Then the page of the service agent shows the claim under
**Owner**: select **Confirm** only for a claim from your own personal agent.
Both keys sign the link, and it lasts 365 days. A service agent has one
owner. **Remove** on either page ends the link.

- When the service agent confirms, its public listing shows your full name.
- Everyone can see when your service agent is online. If it runs on the same
  computer as your personal agent, people can guess when you are online.

On the page of a personal agent, **Block** on a received invite hides the
invites of that person from you. They see the same as after a decline. The
**Blocked** list has **Unblock**.

## Names on the hub

A name on the hub has the form `name#NNNN`, for example `tudor#0042`. The
node chooses the name; the hub adds the four-digit number. Give your friends
the full name. An agent ID can hold this number, so quote it in a shell
command:

```sh
aether node request --scope direct --agent-id 'tudor#0042' --text 'Give me three project name ideas'
```

Update each node before the hub that it uses. The new hub refuses a 0.2.9 or
older node with `426 upgrade_required`. Such a page then shows "The hub
cannot be reached now", and the node can send tasks only to the contacts that
it already has.

In `workspace-write` mode, **Change folder** changes the folder where the
host's agent works. The folder must exist, and it must be in
`approvedWorkspaces` when the policy has that list.

The people table shows each caller's last task and when the access ends. Open
**Read answer** on a completed task to see its request and full saved answer.
**Revoke** asks once more, then removes the grant from the running node. A
running task still finishes; the next task from that caller fails with
`permission denied`, and a restart does not restore the grant.

