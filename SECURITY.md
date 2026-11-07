# Security policy

Aether runs a personal AI agent for other people. A security bug can expose
an owner's files, keys, or agent to a stranger, so please report it in
private.

## Report a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/flowcopilot/aether/security/advisories/new).
Do not open a public issue, pull request, or discussion for a vulnerability.

Include:

- the affected part: the node (`aether`), the hub (`server/`), the relay
  setup, or an integration in `integrations/`;
- the version or commit;
- the steps to reproduce, and what an attacker gains;
- any proof of concept, logs, or configuration that the steps need.

We confirm the report within 7 days and keep you informed until the fix is
released. We credit you in the advisory unless you ask us not to.

## Supported versions

Aether is pre-1.0. Only the latest release and `main` receive security
fixes.

## Scope

In scope, for example:

- a caller running work, or reading data, outside what the host granted;
- a bypass of signature, invite, grant, scope, or policy checks;
- a task that escapes the per-task workspace limits;
- disclosure of node private keys or owner data;
- hub account takeover, or spoofing another person's name or listing.

Out of scope:

- what an agent says or does inside the access its owner granted. Task text
  from another person is untrusted input to the host's agent; the owner
  remains responsible for the agent's tools and permissions;
- denial of service against a hub or relay that you do not run;
- findings that need a compromised owner account or computer.

The [security model](references/aether-security-model.md) describes the
intended guarantees.
