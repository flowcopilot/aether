# TODO: Accept PACT personal agents at an Aether node

See [PLAN.md](PLAN.md).

## Phase 1: Core scope and admission

- [x] Add `ScopeKind::Pact` (`"pact"`) and keep every `match` exhaustive. The fixture refuses a `pact` scope.
- [x] Add `PolicyEngine::admit_verified`: middleware and grant check, no claim, `pact` scope only.
- [x] Make `PolicyEngine::admit` refuse a `pact` scope.
- [x] Tests: each scope kind is refused by the other admission path; a grant still decides.

## Phase 2: PACT store

- [x] `PactStore` with `migrations/pact/001_initial.sql`.
- [x] Contexts: create, read, bind to one Brand user, close, close when idle (deletes their messages).
- [x] Messages: reserve, complete with a stored reply, discard a failed turn, transcript.
- [x] Device authorizations: create, find by user code, poll with `slow_down`, approve, deny, consume. A poll is bound to the agent, its user, and the Brand.
- [x] Grants and refresh tokens: create, spend once, expire.
- [x] Receipts: store and list per grant.
- [x] Tests for each operation and for the single-use rules.

## Phase 3: Keys, JWKS, tokens

- [x] Provider P-256 key, created owner-only, and its JWKS (`kid` is the RFC 7638 thumbprint).
- [x] Delegation token sign and verify (`typ: at+jwt`).
- [x] Receipt sign and verify (`typ: pact-receipt+jws`).
- [x] Personal-agent JWT verification: `alg`, `iss`, `aud` (one string), `sub`, `iat` skew, `exp - iat <= 300`.
- [x] JWKS client: OIDC discovery then `/.well-known/jwks.json`, HTTPS only, no redirects, no proxy, public-address resolver, 64 KiB and 5 s limits, 5-minute cache, 30 s refetch limit, a global fetch rate.
- [x] Tests with local keys, a fake JWKS source, and a loopback HTTP source.

## Phase 4: Identity gateway

- [x] `pact` section in `policy.json`; `pact` scope added; a hand-declared `pact` scope is refused.
- [x] `NodeService::admit_verified` and `NodeService::task_snapshot` (through `ProofNode::admit_verified`).
- [x] Agent Card per Brand and the root redirect.
- [x] `message:send` validation, contexts, retries, step-up without a token.
- [x] Task routes, other A2A operations, error envelope, `401`, `404`/`405`, `429`.
- [x] `node serve --pact-listen` (planned as `--pact-port`).
- [x] Router tests for each spec rule.

## Phase 5: Delegated gateway

- [x] RFC 8414 metadata and the Provider JWKS route.
- [x] Device authorization and token endpoints (device code and refresh token).
- [x] Consent page in the admin UI and the grant it creates.
- [x] Delegated `message:send`: token checks, grant liveness, `sub` lock on the context, Codex turn, receipt.
- [x] Step-up when the token lacks the Brand scope.
- [x] Tests for each OAuth error and for revocation.

## Phase 6: Verification and docs

- [x] End-to-end test with the fake Codex (`tests/pact.rs`): step-up, device flow, consent in the admin UI, delegated turn, receipt checked against the published JWKS, revocation.
- [x] README section.
- [x] Run PACT `e2e/` against the node: 7 of 10 Identity tests pass; the 3 failures are the designed step-up replies. The delegated suite needs the reference Brand and does not apply.

## Open

- [ ] Decide whether a turn without a delegation token should get a plain message instead of the step-up task. That would pass the 3 Identity tests, but it is a change to the "step-up only" decision.
- [ ] Run the gateway behind the real TLS proxy on Coolify.
