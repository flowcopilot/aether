# PACT gateway

A node can also accept personal agents that speak
[PACT 1.0](https://github.com/openpactprotocol/openpactprotocol): A2A 1.0
over HTTPS, with a JWT that identifies the personal agent and OAuth device
code for delegation. The node is a PACT Provider. Each Brand is one
capability that the owner offers. A personal agent gets nothing until the
node owner approves it on the consent page of the local admin UI. The
approval is an ordinary signed grant, so the sharing page shows it and can
revoke it.

Add a `pact` section to `policy.json`. Do not declare a `pact` scope; the
node adds it.

```json
"pact": {
  "publicUrl": "https://aether.example.com",
  "audience": "https://aether.example.com",
  "brands": [{
    "id": "review",
    "name": "Code review",
    "description": "Reviews code in my workspace.",
    "capability": "review",
    "workspace": "workspace",
    "accessMode": "read_only",
    "scope": { "id": "review:read_only", "description": "Review code (read only)" }
  }]
}
```

Start the node with the gateway and the admin UI. The gateway speaks plain
HTTP; put a TLS proxy that serves `publicUrl` in front of it.

```sh
cargo run -p aether -- node serve --policy policy.json --data-dir ./.aether/node \
  --port 0 --admin-port 3000 --pact-listen 127.0.0.1:8080
```

A personal agent reads the Agent Card at
`<publicUrl>/a2a/<brand>/.well-known/agent-card.json` (or at
`<publicUrl>/.well-known/agent-card.json` when there is one Brand). The card
has no `provider`, and its `version` is the version of the card, not of
Aether. Give it the `audience` value; PACT leaves that exchange out of scope. Any HTTPS issuer
that serves a JWKS may call. `allowInsecureLocalIssuers: true` also accepts
`http://localhost` issuers, for local tests only.

The flow:

1. A turn without a delegation token gets `TASK_STATE_AUTH_REQUIRED` with a
   link to `https://127.0.0.1:<admin-port>/pact/consent`. No work runs.
2. The personal agent runs the device flow. The owner opens the link on the
   node machine and allows or denies.
3. With the delegation token, each turn runs as an Aether task in the
   `pact` scope and returns Codex's answer with a signed receipt. The node
   keeps the receipts in `pact.sqlite`.

`cargo test -p aether --test pact` runs this flow end to end with the fake
Codex. PACT's own conformance suite (`e2e/`, `E2E_PROVIDER=any`) passes 7 of
its 10 Identity tests. The other 3 expect a plain message for a turn without
a delegation token, and this node answers with the step-up task on purpose.
Its delegated suite drives the reference Brand's own login page, so it does
not run against another Provider.
