# Plan: Accept PACT personal agents at an Aether node

## Goal

Let a personal agent that speaks [PACT 1.0](https://github.com/openpactprotocol/openpactprotocol)
(`docs/spec.md`, read on 7 October 2026 at commit of 6 October 2026) send
work to an Aether node over HTTPS. The node is a PACT **Provider**. It
implements both profiles of spec §7:

- **PACT Identity** (§2, §3, §4, §6): Agent Cards, the personal-agent JWT,
  `message:send` with `contextId` and retries, the A2A error envelope.
- **PACT Delegated** (§5): device-code OAuth, delegation tokens, step-up, and
  signed receipts.

PACT and Aether already share the A2A 1.0 data model: `a2a-lf` 0.3.0 is
"A2A v1 protocol types". PACT adds an HTTP+JSON binding, JWT identity, and
OAuth delegation. Aether keeps its Iroh binding (`aether/a2a/0`) unchanged.

## Decisions (made on 7 October 2026)

| Question | Decision |
| --- | --- |
| PACT role | Provider gateway. Aether is not a PACT client in this plan. |
| Profile | Identity and Delegated. |
| Brand user and Brand login | The node owner is the only Brand user. Consent opens in the loopback admin UI, which already trusts the one owner session. |
| Caller with only the Identity JWT | Step-up only. No Codex work runs without owner consent. |
| Personal-agent registry | Open. Any HTTPS issuer that serves a JWKS. |

## Mapping

| PACT | Aether |
| --- | --- |
| Provider | One Aether node. A new HTTP listener (`--pact-port`) behind a TLS proxy. Its public base URL is `pact.publicUrl` in `policy.json`. |
| Brand | One entry of `pact.brands` in `policy.json`: a capability, a workspace id, an access mode, and one scope. `brandId` is the entry id. |
| Brand user | `owner`. Delegation tokens carry `sub: "owner"`. |
| Scope | One per Brand, for example `review:read_only`. |
| Grant | A normal signed Aether `Grant`, signed with the node issuer key (`issuer.key`), applied through `Administration::apply_ui`. Its id is the PACT `grant_id`. Revoking it in the admin UI revokes the delegation. |
| Caller | `subject_agent_id = "pact:" + iss`, `subject_endpoint_id = "pact:" + iss + "#" + blake3(sub)`. |
| Turn | One Aether task in a new scope kind `pact`. The node admits it with the same grant check, middleware, queue limits, and scheduler as an Iroh task. |
| Provider signing key | A new P-256 key, `<data-dir>/pact-signing.key` (PKCS#8, owner-only). It signs delegation tokens and receipts. ES256 only. Aether's Ed25519 keys do not change; PACT does not accept EdDSA (§3.2). |
| Receipt | ES256 compact JWS of the §5.6 claims. The node also stores each receipt for the later trust layer. |

### Interpretations of the spec

These are our readings, not text from the spec.

1. **Brand login.** §5.3 says the Brand's login returns the User to the
   Provider "with a single-use assertion bound to the `user_code`, sent by
   `POST`". Here the Brand and the Provider are the same process, so the
   assertion is internal: the consent page is a page of the admin UI, behind
   the admin session, and the decision is a same-origin `POST`.
2. **Verification URI.** `verification_uri` is
   `https://127.0.0.1:<admin-port>/pact/consent`. The owner must open it on
   the node machine. This follows the "owner, loopback UI" decision.
3. **JWKS discovery for an open registry.** §3.1 says `jwksUri` "MAY then be
   found through OIDC discovery at `{iss}/.well-known/openid-configuration`".
   The reference personal agent serves only `/.well-known/jwks.json`. The node
   tries OIDC discovery first, then `{iss}/.well-known/jwks.json`.
4. **Grant binding.** The grant is bound to the personal agent's `iss` **and**
   `sub`. The spec requires only `client_id == iss` (§5.5). The extra check
   stops another user of the same personal agent from using the owner's grant.
   The device code is bound the same way: a poll from another `sub` gets
   `invalid_grant` and does not consume the approval.
5. **Context transcript.** Aether tasks are single-turn. A turn in a
   continued context sends the earlier turns to Codex as text, oldest first,
   cut to half of `maxContextTokens`.

### Known divergence from the conformance suite

Corrected on 7 October 2026 after the run: **three** tests fail by design,
not two.

`e2e/pact.test.ts` "returns Message replies…", "returns the same reply for a
duplicate messageId", and "rejects contexts owned by another user or
customer" expect a `message` for a turn sent with only the Identity JWT. The
last one reads the reply through `A2AClient.sendMessage`, which accepts only
a `message`. With "step-up only", the node returns a
`TASK_STATE_AUTH_REQUIRED` task. Spec §4.2 allows "a task, §5.5", and the
reference Provider also steps up when no token is present
(`reference/provider/src/delegation/agent.ts`).

Measured on 7 October 2026 with `E2E_PROVIDER=any` against
`node serve --pact-listen`: 7 of 10 Identity tests pass, and the 3 above
fail. `e2e/delegated.test.ts` cannot run against another Provider: it
expects the Skyline scope ids (`flights:*`) and logs in to the reference
Brand with an e-mail and a password. `crates/aether/tests/pact.rs` covers the
delegated flow instead.

The card names the JWT scheme `platformJwt`. The spec's examples write
`paJwt`, but the name is not normative and the suite reads `platformJwt`.

## Security

- **SSRF.** An open registry makes the node fetch URLs that callers choose.
  The JWKS client accepts only `https` URLs, follows no redirects, resolves
  names through a resolver that refuses loopback, private, link-local,
  shared, documentation, and multicast addresses, reads at most 64 KiB, and
  stops after 5 s. `pact.allowInsecureLocalIssuers` (default `false`) allows
  `http://localhost` and `http://127.0.0.1` issuers for local tests only.
- **Fetch amplification.** Keys are cached for 5 minutes per issuer. An
  unknown `kid` causes at most one refetch per issuer per 30 s. The cache
  holds at most 1024 issuers.
- **Rate limits.** A per-issuer `governor` limit on every authenticated
  route returns `429` with `Retry-After`. A global connection limit, a 64 KiB
  body limit, and a request time limit apply to the listener.
- **Fail closed.** Any failure to verify a personal-agent JWT is `401` with
  `WWW-Authenticate: Bearer realm="a2a"` and no body (§3.4). A bad
  delegation token adds `error="invalid_token"` (§5.5).
- **Scope isolation.** `PolicyEngine::admit` refuses a `pact` scope, and the
  new `PolicyEngine::admit_verified` accepts only a `pact` scope. A PACT
  caller cannot reach a direct, directory, or managed scope, and an Iroh peer
  cannot reach the `pact` scope.
- **Secrets at rest.** Device codes and refresh tokens are stored as BLAKE3
  digests. Refresh tokens are single use and rotate.
- **Consent page.** It needs the admin session, checks the loopback `Host`,
  and sends `frame-ancestors 'none'`. It shows the personal agent's issuer
  origin, the Brand, and each scope as a checkbox.

## Design

### Policy file

```json
"pact": {
  "publicUrl": "https://aether.example.com",
  "audience": "https://aether.example.com",
  "allowInsecureLocalIssuers": false,
  "brands": [
    {
      "id": "review",
      "name": "Code review",
      "description": "Reviews code in the owner's workspace.",
      "capability": "review",
      "workspace": "workspace",
      "accessMode": "read_only",
      "scope": { "id": "review:read_only", "description": "Review code in the workspace (read only)" }
    }
  ]
}
```

The `pact` scope (`{"id": "pact", "kind": "pact"}`) is added to the node's
scopes when `pact` is present; a policy that declares a `pact` scope by hand
is refused. `node serve --pact-listen <address:port>` starts the listener; it
needs `--admin-port`, because consent lives there. (The flag was planned as
`--pact-port`; an address lets a container bind `0.0.0.0`.)

### Routes

All under `pact.publicUrl`. `{i}` is the interface URL,
`{publicUrl}/a2a/{brandId}`.

| Method and path | Spec |
| --- | --- |
| `GET /.well-known/agent-card.json` | §2.1: redirects to the card when exactly one Brand exists |
| `GET {i}/.well-known/agent-card.json` | §2.1, §5.1 |
| `POST {i}/message:send` | §4, §5.5, §5.6 |
| `GET {i}/tasks`, `GET {i}/tasks/{id}`, `POST {i}/tasks/{id}:cancel` | §2.2 |
| other A2A operations | §2.2, §6: their A2A errors |
| `GET {i}/oauth/.well-known/oauth-authorization-server` | §5.1, RFC 8414 |
| `GET {publicUrl}/a2a/jwks.json` | §5.1: the Provider keys |
| `POST {i}/oauth/device_authorization` | §5.3, RFC 8628 |
| `POST {i}/oauth/token` | §5.3: device code and refresh token |

Admin UI (loopback): `GET /pact/consent` and `POST /pact/consent`.

### Storage

A new SQLite file, `<data-dir>/pact.sqlite`, with its own migrations in
`aether-store` (`PactStore`). It holds contexts, messages and their stored
replies, device authorizations, PACT grants, refresh-token digests, and
receipts. It does not change `node.sqlite`. A context closes after 24 hours
without a message; maintenance then deletes its message text.

### Code layout

- `aether-core`: `ScopeKind::Pact`, `PolicyEngine::admit_verified`.
- `aether-store`: `PactStore` and `migrations/pact/`.
- `aether`: `pact/` module — `config.rs`, `keys.rs`, `jwks.rs`,
  `tokens.rs`, `gateway.rs`, `consent.rs`; `admin_ui/pact.rs` for the page;
  `NodeService::admit_verified` and `NodeService::task_snapshot`; `node serve`
  flags.

### New dependency

`jsonwebtoken` 11 with `default-features = false, features = ["aws_lc_rs"]`
(MIT, 205 M downloads). `aws-lc-rs` is already in `Cargo.lock` through
`rustls`, so no new crypto library enters the build. Key generation uses
`aws-lc-rs` directly (`EcdsaKeyPair::generate_pkcs8`), so `aws-lc-rs` becomes
a direct dependency at the version the lockfile already holds.

## Phases

Each phase builds, passes `mise run check`, and ends in one scoped
Conventional Commit.

1. **Core scope and admission.** `ScopeKind::Pact`; `admit_verified`; `admit`
   refuses `pact`; plugin match. Commit `feat(core): add the pact scope kind
   and verified admission`.
2. **PACT store.** `PactStore` and its migrations, with tests. Commit
   `feat(store): add the PACT state store`.
3. **Keys, JWKS, tokens.** Provider key, JWKS document, delegation token,
   receipt, personal-agent JWT verification with the guarded JWKS client.
   Commit `feat(pact): verify personal-agent JWTs and sign delegation tokens`.
4. **Identity gateway.** Card, `message:send` (step-up without a token),
   task routes, errors, policy config, `NodeService` methods, `node serve`
   flag. Commit `feat(pact): serve the PACT Identity profile`.
5. **Delegated gateway.** OAuth metadata, device authorization, token
   endpoint, consent page, grant creation, delegated turns, receipts. Commit
   `feat(pact): serve the PACT Delegated profile`.
6. **Verification and docs.** An end-to-end test with a fake Codex: device
   flow, consent, delegated turn, receipt check, revocation. README section.
   Run PACT's own `e2e/` suite against the node when Node.js is available.
   Commit `test(pact): verify the delegated flow end to end` and `docs(pact):
   describe the PACT gateway`.

## Out of scope

Aether as a PACT client, more than one Brand user, consent from outside the
node machine, consent skipping for an existing grant (§5.3 "MAY"), streaming,
push notifications, and per-Brand subdomains for the consent page.
