# TODO: Discovery Hub for personal and service agents

## Phase 1: Remove the build data from the PACT Agent Card

- [x] Remove `"provider"` from `card()` in `crates/aether/src/pact/gateway.rs`
- [x] Add `PACT_CARD_VERSION` (`"1"`) and use it for `"version"`
- [x] README PACT section: the card has no provider and a card version
- [x] Test: `the_agent_card_advertises_both_profiles_and_needs_no_token` checks the version and the exact top-level key allowlist
- [x] `cargo test -p aether --lib pact::gateway` passes (the changed unit test)
- [x] `cargo test -p aether --test pact` passes (end-to-end)
- [x] `mise run check` passes
- [x] Commit: `fix(pact): drop the Aether provider and build version from the Agent Card`

## Phase 2: Remove host runtime text from caller-facing descriptions

- [x] `aether.task.send` description has no folder text (`crates/aether/src/mcp.rs`)
- [x] Caller page title has no folder text (`crates/aether/src/admin_ui/caller.rs`)
- [x] Host `workspace_line` text stays as it is
- [x] Hermes `plugin.yaml`, `schemas.py`, `tools.py` and `README.md` do not name Codex or "real"
- [x] Tool names, plugin config schema and bridge protocol do not change
- [x] `python3 -I -m py_compile` passes for the three plugin `.py` files
- [x] `mise run check` passes
- [x] Commit: `fix(node): remove host runtime text from caller-facing descriptions`

## Phase 3: Stop naming the agent after the login account

- [x] Delete `host_name` and the `$USER` fallback in `crates/aether/src/main.rs`
- [x] `--agent-id` keeps its directory role only (doc comment); `start_directory_publication` does not change
- [x] New `node serve` flag `--invite-name`; an invalid value exits with code 2
- [x] `HostContext` keeps `name: Option<String>`; add `agent_name()` (hub name, `--invite-name`, `agent-` + 8 characters)
- [x] Add `direct::default_agent_name`
- [x] Add `node_config::{HUB_NAME_KEY, stored_hub_name, store_hub_name}`
- [x] `hub.rs` `load()` and `claim_name` keep the hub name; `hub_url` clears it on a hub change
- [x] `host.rs` uses `host.agent_name()` in `create_invite` and the two `<code>` lines
- [x] `README.md:214` names the new rule; `README.md:14-17` has no exact pass counts
- [x] `INSTA_UPDATE=always cargo test -p aether --test commands` updates `commands__help_node_serve.snap` and `commands__usage_spec.snap`; `git diff crates/aether/tests/snapshots/` read
- [x] Test: `the_agent_name_comes_from_the_hub_name_then_the_invite_name_then_the_endpoint`
- [x] Test: `node_serve_starts_with_only_an_invite_name`
- [x] Test: `an_invitation_applies_a_grant_and_the_host_sees_the_agent_answer` still names `tudor`
- [x] `mise run check` passes
- [x] Commit: `fix(node): stop naming the agent after the login account`

## Phase 4: Round presence on the hub

- [x] `verify.ex`: rate limit before `Presence.seen`
- [x] `hub.ex` `list_contacts/2`: `lastSeen` rounded down to 900 s
- [x] `presence.ex`: hourly `sweep/1` deletes entries older than 7 days
- [x] Remove the unused `Contact.last_seen` in `crates/aether/src/hub_client.rs`
- [x] `server/PROTOCOL.md` `GET /v1/contacts` describes rounded `lastSeen`
- [x] Tests: updated presence tests; "lastSeen is rounded down to 15 minutes"; "the presence sweep drops entries older than 7 days"; "a rate-limited request does not mark the node as seen"
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] `mise run check` passes
- [x] Commit: `fix(server): round contact lastSeen and record presence after the rate limit`

## Phase 5: Decline MCP elicitation requests

- [x] `approval_request` matches `mcpServer/elicitation/request`
- [x] `deny_approval` replies `{"action": "decline"}` for it
- [x] Test: `app_server_declines_an_mcp_elicitation_request`
- [x] Test: `app_server_denies_and_reports_approval_requests` still passes
- [x] `mise run check` passes
- [x] Commit: `fix(codex): decline MCP elicitation requests`

## Phase 6: Pin the agent MCP config in the node Codex home

- [x] Shared setup helper in `crates/aether/tests/commands.rs`
- [x] Test: `a_node_task_runs_codex_with_the_agent_mcp_config` (agent config used, owner `CODEX_HOME` ignored, approval `never`)
- [x] Test: `codex_runs_with_the_node_codex_home_not_the_owner_home` checks that `config.toml` is not copied and survives a second `prepare_codex_home`
- [x] Extract `initialize_params()` in `crates/aether/src/execution.rs`
- [x] Ignored test: `live_app_server_lists_the_agent_mcp_servers` (`AETHER_RUN_LIVE_CODEX=1`)
- [x] Run the live test once with codex-cli 0.162.1
- [x] `mise run check` passes
- [x] Commit: `test(codex): pin the agent MCP config in the node Codex home`

## Phase 7: Document the agent's MCP servers and what a node never publishes

- [x] `README.md:53-56` names `<data-dir>/codex-home/config.toml` as the agent's tools
- [x] `README.md` section "Agent tools (MCP servers)" has each point of PLAN Phase 7:
  - [ ] 1. The path `<data-dir>/codex-home/config.toml`
  - [ ] 2. Example: absolute command, `env` table, `enabled_tools`, `approval_mode`
  - [ ] 3. `CODEX_HOME=<data-dir>/codex-home codex mcp add|list|remove`; `enabled = false`
  - [ ] 4. Effect of approval `"never"` and of `approval_mode = "approve"`
  - [ ] 5. Elicitations are declined
  - [ ] 6. No `PATH` unless allowlisted
  - [ ] 7. MCP servers run outside the task sandbox
  - [ ] 8. Every admitted caller can use every enabled tool
  - [ ] 9. A change applies to the next task
  - [ ] 10. `--strict-config`
  - [ ] 11. Connectors and plugins are off
  - [ ] 12. Do not add `aether mcp serve` to this file
  - [ ] 13. Secrets go in the `env` table
  - [ ] 14. D1: Aether never publishes these servers; codex-cli 0.162.1 checked
- [x] `README.md` task folder paragraph: MCP servers are outside the sandbox
- [x] `README.md:21-26` names the two real ignored tests
- [x] Doc comments: `RESERVED_ENVIRONMENT`, `prepare_codex_home`, the `"never"` value, `DISABLED_CODEX_FEATURES`
- [x] `references/aether-security-model.md`: one-text-part rule, "Agent tools", "What a node never publishes"
- [x] `references/aether-wire-profile.md` "A2A subset" matches `validate_payloads`
- [x] `references/a2a-codex-connection-flow.md`: the two MCP roles
- [x] Manual: the README example passes `codex mcp list` in a temp `CODEX_HOME`
- [x] `rg -n 'text and JSON parts' references/` returns nothing
- [x] `mise run check` passes
- [x] Commit: `docs(node): document the agent's MCP servers and what a node never publishes`

## Phase 8: Add the general skill to new grants and Codex instructions

- [x] Add `execution::GENERAL_SKILL` and `developer_instructions`
- [x] `thread_start_params` uses `developer_instructions`
- [x] `sign_invite` signs `GENERAL_SKILL`
- [x] `NodeService::handle` sets the `Task` skill to `GENERAL_SKILL` for a `review` capability in the direct scope; the signed grant and `request.capability_id` do not change; PACT tasks (`pact/gateway.rs`) keep their Brand capability
- [x] `direct.rs` test helper `bundle_for` uses `GENERAL_SKILL`
- [x] Docs: security model and README invite section
- [x] Test: `app_server_requests_keep_skill_and_remote_text_separate` checks both sentences
- [x] Test: `the_general_skill_asks_for_the_sent_task_and_keeps_the_setup_private`
- [x] Test: the invite test checks `capability_id` and the grant capabilities
- [x] Test: `a_direct_review_grant_runs_with_the_general_instructions` (skill `general`, capability `review`)
- [x] Existing `node_service.rs` tests: `general:remote text` in the two `handle` tests; skill `general` and its fingerprint in the four tests that store a task before `handle`
- [x] `mise run check` passes
- [x] Commit: `feat(node): add the general skill to new grants and Codex instructions`

## Phase 9: Sign hub queries exactly as sent

- [x] `HubClient::send(method, path, query, body)` builds the URL, then signs `signed_target(&url)`
- [x] `query_pairs_mut()` only for a non-empty query; `signed_target` adds `?` only for a non-empty query
- [x] `test_hub` builds the signed target as Plug does (no trailing `?`)
- [x] Test: `a_request_without_a_query_is_signed_as_the_bare_path`
- [x] All callers updated; `invites()` sends `after` and `wait` as pairs
- [x] `send` adds `x-aether-protocol: 2` to every request; the header is not in the signed message
- [x] `Status::Unknown` (`#[serde(other)]`); `hub_section` drops it
- [x] `segment()` doc comment says 32 hex characters
- [x] Add `crates/aether/src/hub_client/test_hub.rs`
- [x] Test: `a_free_text_query_is_signed_as_it_is_sent`
- [x] Test: `the_hub_receives_the_signed_path_and_query`
- [x] Test: `an_unknown_invite_status_does_not_break_the_list`
- [x] `test_hub` records the headers of each request
- [x] Test: `every_hub_request_carries_the_protocol_header`
- [x] `mise run check` passes
- [x] Commit: `fix(node): sign hub queries exactly as sent`

## Phase 10: Add a trusted proxy client IP and rate limit buckets

- [x] New `AetherHub.ClientIp` plug with `parse_cidrs!/1`
- [x] `TRUSTED_PROXIES` and `NEW_ACCOUNTS_PER_IP_HOUR` in `config/runtime.exs`
- [x] `application.ex` parses the list at boot; a bad value stops the boot
- [x] `RateLimit.hit(bucket, key)` with the six default buckets
- [x] `verify.ex` uses the `:request` bucket
- [x] Router: `ClientIp` before `Verify`; `rate_limited: 429`
- [x] First claim counts in `:new_account_ip`
- [x] Test client: `:remote_ip` and `:headers`; IP from the node key
- [x] `server/README.md` and `server/PROTOCOL.md` limits; "Behind a proxy" says `TRUSTED_PROXIES` is required before the deploy, and gives the Coolify Traefik steps (`docker inspect coolify-proxy`, the shared network subnet, the CDN note)
- [x] Tests: `client_ip_test.exs`, `rate_limit_test.exs`, two new `limits_test.exs` cases; the 120-request test stays green
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server): add trusted proxy client IP and rate limit buckets`

## Phase 11: Show the sender endpoint on a delivered invite

- [x] `invite_json/4` adds `senderEndpoint` for the receiver of a delivered row only
- [x] `@bundle_max` stays 8 KiB
- [x] `server/PROTOCOL.md`: invite object and the receiver rule
- [x] Test: the full-flow test checks `senderEndpoint` at each status for both roles
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server): show the sender endpoint on a delivered invite`

## Phase 12: Sign hub invites only with a local approval

- [x] Migration `012_hub_approvals.sql` in `NODE_MIGRATIONS`
- [x] `HubApproval`, `ApprovalSide`, `ApprovalKind` and five store methods
- [x] `crates/aether/src/hub_sync.rs` with `host_step`; `own_name` is `node_config::stored_hub_name`, never a flag
- [x] `invite_by_name` writes a `Sender` approval; removes the hub row when the write fails
- [x] `host_worker` uses `host_step`; pins the line; sends the kept line again; refuses changed rows
- [x] `invite_row` text for a row without an approval
- [x] `server/PROTOCOL.md` "Trust" sentence
- [x] Tests: two store tests, three `hub_sync` tests, `a_hub_row_the_owner_never_sent_gets_no_grant`, `a_host_with_an_invite_name_still_signs_its_own_hub_invite`
- [x] `mise run check` passes
- [x] Commit: `fix(admin-ui): sign hub invites only with a local approval`

## Phase 13: Remove a contact from the page and the CLI

- [x] `NodeStore::remove_signed_record(id)` in `crates/aether-store/src/lib.rs`
- [x] `direct::remove_contact` deletes `direct:<agent_id>` and `caller.contact.<agent_id>`
- [x] Caller page **Remove** form; `.page(caller::remove_contact)` and `/contacts/remove` in `router()`
- [x] `node direct remove`; snapshots updated with `INSTA_UPDATE=always cargo test -p aether --test commands`; `git diff crates/aether/tests/snapshots/` read
- [x] Tests: `remove_signed_record_deletes_one_record_and_reports_a_missing_one`, `remove_contact_forgets_the_bundle_and_the_record`, `a_caller_removes_a_contact_from_the_page`
- [x] `mise run check` passes
- [x] Commit: `feat(direct): remove a contact from the page and the CLI`

## Phase 14: Check hub bundles against the sender name and endpoint

- [x] `Invite.sender_endpoint`
- [x] `DirectError::NameInUse`; `import_invitation` refuses a name from another endpoint
- [x] `contact_by_endpoint`
- [x] `import_bundle` removes an old contact with the same endpoint (`remove_contact`)
- [x] `hub_sync::caller_step`
- [x] `accept_invite` writes a `Receiver` approval before `hub.accept`
- [x] `caller_worker` uses `caller_step`
- [x] Caller page shows `NameInUse`; exit code 2 for `NameInUse`
- [x] `test_hub` sends `senderEndpoint` on a delivered row to the receiver
- [x] Tests: `caller_step` cases, rename replace, name in use, `test_hub` import check
- [x] `mise run check` passes
- [x] Commit: `fix(direct)!: check hub bundles against the sender name and endpoint`

## Phase 15: Name hub accounts name#NNNN with a fixed agent type

- [x] Migration `20261016000000_name_accounts_with_numbers.exs` (`down` raises; raw SQL, no schema module); `released_names` keeps `kind`
- [x] `account.ex` fields; new `released_name.ex`
- [x] New `AetherHub.Names`; `pick_number` takes the first preferred number that is free for this key
- [x] `put_account/3`: only `name`, `kind` and `searchable` (another key is `:invalid`); `kind` once (also across released tags of the endpoint); `searchable` for personal; partial later PUT
- [x] Rename keeps the current number when free, else this key's released number, else random; one transaction
- [x] `account_json/1`, `create_invite/2`, snapshots and contacts use full names; contacts add `kind`
- [x] `create_invite/2`: a `service` receiver gets `:not_found`; a `service` sender gets `:forbidden`
- [x] `GET /v1/hub` answers `{"protocol": 2}` without an account
- [x] `verify.ex`: a `/v1` request without `x-aether-protocol: 2` gets 426 `upgrade_required` before the body read and the signature check; `GET /v1/hub` does not need the header
- [x] `@statuses` in `router.ex`: `upgrade_required: 426`
- [x] Test client `unique_name/0` and `claimed_node/1`; `request/5` sends `x-aether-protocol: 2` by default, `protocol: nil` leaves it out
- [x] `server/PROTOCOL.md` "Accounts" rewritten; `GET /v1/hub` described; "Request signing" names the protocol header; "Errors" has 426 `upgrade_required`
- [x] Tests in `auth_test.exs`: "a /v1 request without the protocol header gets 426 upgrade_required"; "GET /v1/hub answers without the protocol header"; "a request without the headers is refused" sends the protocol header and still gets 401
- [x] Tests: rewritten `account_test.exs` (rename order, `GET /v1/hub`); updated `invites_test.exs` (service receiver and sender) and `contacts_test.exs`
- [x] Test: `migration_test.exs` runs on a dynamic repo migrated `to: 20261015120000`, inserts old rows with raw SQL, runs the names migration, checks name, number, kind, version and unique pairs
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server)!: name hub accounts name#NNNN with a fixed agent type`

## Phase 16: Accept name#NNNN agent names

- [x] `direct::valid_name` accepts one `#` and 4 digits
- [x] `access_groups::valid_group_name` keeps the old rule
- [x] `hub_client`: `valid_name_part`, `split_full_name`, `valid_full_name`, `ClaimBody { name }`
- [x] `hub_client::protocol()` (`GET /v1/hub`; 404 is protocol 1) and `HubError::OldHub`; `claim()` checks it first
- [x] `HubError::OldNode`; `status_error` maps 426 to it
- [x] `--invite-name` uses `valid_name_part` (no `#`)
- [x] Hub forms: name hint, `to` pattern and placeholder, messages (`OldHub` and `OldNode` texts)
- [x] Caller page: `request-{index}` ids, quoted commands, "full name" title
- [x] Host page: `friend_name` pattern and quoted commands
- [x] `README.md`: names and quoting; update the node before the hub deploy
- [x] `test_hub`: `GET /v1/hub`, `old` mode, `{name}` claim body, full names in invites
- [x] Tests: eight tests named in the plan, with `an_old_hub_shows_the_older_hub_text_and_gets_no_claim` and `status_error_maps_upgrade_required_to_old_node`
- [x] `mise run check` passes
- [x] Commit: `feat(direct)!: accept name#NNNN agent names`

## Phase 17: Add hub account deletion and hidden blocks

- [x] Migration `20261016010000_create_blocks.exs` with `invites.hidden_from`
- [x] `AetherHub.Blocks` with the 1,000 cap
- [x] Hidden rows in `create_invite/2`, `snapshot/1`, `open_invite!/2`, `delete_invite/2`, `counterparts/1`
- [x] `PUT`, `GET`, `DELETE /v1/blocks`; `POST /v1/invites/{id}/block` for the receiver of an open invite; `DELETE /v1/account`; `limit: 409`
- [x] `delete_account/1` releases the tag with its `kind`
- [x] `server/PROTOCOL.md` "Blocks" and `DELETE /v1/account`
- [x] Tests: `blocks_test.exs` (seven cases, with block by invite id); three deletion cases in `account_test.exs` (with "a claim with the other kind after deletion gets 400")
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server): add account deletion and hidden blocks`

## Phase 18: Choose a personal or service agent at name claim

- [x] `AgentKind`; `Account.kind` and `searchable`; `kind` only on the first claim; `set_searchable`
- [x] `node_config` agent kind key; a different kind is refused
- [x] Name form radios ("Make only one for each person"); caller-only page sends `personal`
- [x] `load()` stores the hub kind; an old hub is found only by `protocol()`; `Personal` is only a parse fallback
- [x] `hub_section` by kind; no invite-by-name form and no invite rows for a service agent
- [x] `POST /hub/searchable` for personal agents; default off; `.page(hub::set_search)` and `/hub/searchable` in `router()`
- [x] `README.md`: the two agent types; one personal agent for each person (not checked by the hub)
- [x] `test_hub`: `kind` and `searchable` on the account routes
- [x] Tests: five tests named in the plan
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): choose a personal or service agent at name claim`

## Phase 19: Block invite senders on the personal page

- [x] `HubError::Limit`; `status_error` maps 409 `limit` before the generic 409 arm
- [x] `hub_message` text for `Limit`
- [x] Hub client `block_invite(id)`, `blocks()`, `unblock(endpoint)`
- [x] Personal page: **Block** on received invite rows; "Blocked" list with **Unblock**
- [x] `.page(hub::block_invite)`, `.page(hub::unblock)`; `/hub/block` and `/hub/unblock` in `router()`
- [x] `test_hub`: block by invite id, block list, unblock, hidden rows, `limit`
- [x] `README.md`: Block on the personal page
- [x] Tests: `status_error_maps_limit_apart_from_taken`, `a_blocked_invite_sender_and_its_new_invites_do_not_show`, `a_full_block_list_shows_the_limit_message`
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): block invite senders on the personal page`

## Phase 20: Keep an owner-written Agent Card on the node

- [x] `crates/aether/src/untrusted.rs` (`single_line`, `multi_line`, `chunk`, `UNTRUSTED_NOTE`)
- [x] `crates/aether/src/agent_card.rs`: settings in `node.agent_card`, shared limits, `MAX_CARD_BYTES`
- [x] `save_settings`, `compose`, `validate`, `CardView`
- [x] `discovery::sign_agent_card` and `verify_agent_card`
- [x] No code shared with the PACT card
- [x] Tests: seven tests named in the plan
- [x] `mise run check` passes
- [x] Commit: `feat(node): keep an owner-written Agent Card on the node`

## Phase 21: Edit the Agent Card on the host page

- [x] `card_section` and `POST /card` with `CardForm`
- [x] Page and route registered in `crates/aether/src/admin_ui.rs`
- [x] `README.md` "Your agent's card"
- [x] Test: `the_owner_edits_the_agent_card_on_the_host_page`
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): edit the Agent Card on the host page`

## Phase 22: Accept aether-invitation/v2 with a signed Agent Card

- [x] `BUNDLE_FORMAT_V1`, `BUNDLE_FORMAT_V2`; all uses updated
- [x] Card fields, `v1` and `v2` constructors, strict `decode`
- [x] `import_bundle` checks the card before any store write
- [x] Contact bundle kept as `v1`; card under `caller.card.<endpoint>`; `v1` import and `remove_contact` delete it
- [x] `contact_card` and `test_bundle`
- [x] Tests: four new tests; `v1` tests still pass
- [x] `mise run check` passes
- [x] Commit: `feat(direct): accept aether-invitation/v2 with a signed Agent Card`

## Phase 23: Send the Agent Card with each new invite

- [x] `direct::create_card`
- [x] `sign_invite` makes the card before `apply_ui` and builds `v2`; card name is `host.agent_name()` (hub name first)
- [x] Docs: README invite steps and the `v2` note for 0.2.8 callers, security model, `server/PROTOCOL.md` bundle limit
- [x] Tests: invite test checks `v2` and card name; `the_card_names_the_hub_name_when_the_node_has_one`; caller import of `v2`; `the_longest_card_keeps_the_invite_line_under_the_hub_limit`
- [x] `mise run check` passes
- [x] Commit: `feat(node)!: send the Agent Card with each new invite`

## Phase 24: Show each agent's card to the caller

- [x] `agents_list` adds `card` as untrusted owner text, no interface or endpoint
- [x] New `aether.agents.list` description
- [x] Caller page shows the description and skill name
- [x] Tests: `agents_list_shows_each_card_as_untrusted_owner_text`; caller page shows the default description
- [x] `mise run check` passes
- [x] Commit: `feat(node): show each agent's card to the caller`

## Phase 25: Add a sent tasks table for the outbox and inbox

- [x] Migration `013_sent_tasks.sql` in `NODE_MIGRATIONS`
- [x] `SentState`, `SentTaskInput`, `SentTask`, `SentAttempt`
- [x] Fourteen `NodeStore` methods; claims in `IMMEDIATE` transactions
- [x] No unchecked arithmetic or slicing
- [x] Tests: seven tests named in the plan
- [x] `mise run check` passes
- [x] Commit: `feat(store): add a sent tasks table for the outbox and inbox`

## Phase 26: Add one-exchange requests and resend frames

- [x] `transport::relay_endpoint_addr`, `ip_endpoint_addr`, `exchange_on`
- [x] `RequestIds: Clone`; `resend_frame`, `contact_by_name`, `access_ends_at`
- [x] `aether node request` keeps its loop
- [x] Tests: three tests named in the plan
- [x] `mise run check` passes
- [x] Commit: `feat(direct): add one-exchange requests and resend frames`

## Phase 27: Add an outbox that sends tasks again and keeps answers

- [x] `crates/aether/src/outbox.rs` with constants, `Courier`, `RelayCourier`, `OutboxError`
- [x] `enqueue`, `outcome`, `next_delay`, `attempt`, `run_once`, `run`
- [x] `hub_client::configured` and `presence` moved from `mcp.rs`
- [x] Tests: seven unit tests and `the_outbox_sends_again_to_a_full_host_queue_and_keeps_the_answer`
- [x] `mise run check` passes
- [x] Commit: `feat(node): add an outbox that sends tasks again and keeps answers`

## Phase 28: Run the outbox in node serve

- [x] `Administration` outbox waker; `AdminError::Unsupported("outbox")` without one (`node admin` answers `unsupported`)
- [x] `Request::OutboxWake`
- [x] `node serve` spawns `outbox::run` on its own endpoint; stops it before `endpoint.close()`
- [x] Tests: `outbox_wake_reaches_the_node_and_is_unsupported_without_one`; `node_serve_ends_an_expired_outbox_task_after_a_wake`
- [x] `mise run check` passes
- [x] Commit: `feat(node): run the outbox in node serve`

## Phase 29: Queue sent tasks and add inbox tools

- [x] `McpServer` `hub_flag` and `send_wait`; `run_mcp` passes the flag
- [x] New `task_send` with the short wait and the one bounded fallback attempt
- [x] Fallback only when the socket file is missing, the connection is refused, or the answer is `unsupported`; `invalid` (old `node serve`) keeps the row waiting and asks for a restart
- [x] Every result has `status` (`answered`, `failed`, `not_delivered`, `waiting`)
- [x] `aether.inbox.list` and `aether.inbox.read`
- [x] New descriptions and instructions; answers inside `untrusted`
- [x] Remove `direct::ask` and `direct::AskError`
- [x] `.codex/config.toml` `tool_timeout_sec = 120`
- [x] Tests: eight tests named in the plan, with `task_send_does_not_bind_a_second_endpoint_when_an_old_node_answers_invalid`
- [x] `mise run check` passes
- [x] Commit: `feat(mcp): queue sent tasks and add inbox tools`

## Phase 30: Show sent tasks on the caller page

- [x] "Sent tasks" section with status words and the answer in `<details>`
- [x] MCP hint names `aether.inbox.read`
- [x] Test: `caller_page_shows_a_not_delivered_task_and_an_answer`
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): show sent tasks on the caller page`

## Phase 31: Describe the outbox and the inbox tools

- [x] `README.md` MCP sections
- [x] `references/aether-task-state.md` "Sender outbox"
- [x] `RELAY-TEST.md` manual relay check
- [ ] Run the `RELAY-TEST.md` check once (not run here: it needs two computers with Codex logins and the public relay)
- [x] Commit: `docs: describe the outbox and the inbox tools`

## Phase 32: Add people lookup on the hub

- [x] `AetherHub.People` `lookup/4` and `search/4`
- [x] Routes `GET /v1/people/:name/:number` and `GET /v1/people` with search buckets
- [x] `server/PROTOCOL.md` "People"
- [x] Tests: `people_test.exs` (seven cases)
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server): add people lookup by exact name and opt-in name search`

## Phase 33: Add signed service listings with FTS5 search

- [x] Migration `20261016030000_create_listings.exs` with `listing_search`
- [x] `AetherHub.Signature`
- [x] `AetherHub.Listings` `put`, `search`, `lookup`, `get`, `delete`, `delete_expired`
- [x] Allowlist with the shared limits; no control, bidirectional or zero-width characters; exact string kept
- [x] Rename and account deletion remove the listing; the sweeper removes expired ones
- [x] Five routes
- [x] `server/PROTOCOL.md` "Listings" and "Search"
- [x] Tests: `listings_test.exs` and `agents_search_test.exs`
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server): add signed service listings with FTS5 search`

## Phase 34: List a service agent on the hub

- [x] `crates/aether/src/listing.rs` (`listing_bytes`, `sign`, `parse`, `verify`)
- [x] `node.listing_state`; hub client listing methods
- [x] `listing::verify` checks the expiry against a given time
- [x] `node_config::queue_limits` (stored policy with the `admin.limit.*` overrides; `None` without a policy)
- [x] `listing_section` with the preview, online text, queue text from `queue_limits` and IP note
- [x] `POST /listing/publish`; 400 for a personal agent; `.page(listing::publish_listing)` and `/listing/publish` in `router()`
- [x] `listing_worker` renews every 6 h
- [x] `README.md`: listing, IP note, shared queue
- [x] `test_hub`: `PUT`, `GET`, `DELETE /v1/listing`
- [x] Tests: six tests named in the plan, with `the_queue_text_shows_the_limits_in_force`
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): list a service agent on the hub`

## Phase 35: Add aether.agents.find and aether.people.find

- [x] Hub client `find_agents`, `agent`, `person`, `find_people`
- [x] `aether.agents.find` with `untrusted` text and no endpoint
- [x] `aether.people.find` in the D8 order
- [x] Join rule: `contact` and `access: ready` only when the hub row `endpoint` equals a local contact `endpoint_id`; a name match with another endpoint gives `none` and a `warning`
- [x] Old hub (`protocol()` is 1) or no hub: "the hub does not support search"; a 404 on a find route is "not found"
- [x] `test_hub` routes for agents and people
- [x] Tests: eight tests named in the plan, with `find_tools_join_local_contacts_by_endpoint_not_by_name` and `find_tools_on_an_old_hub_say_that_search_is_not_supported`
- [x] `mise run check` passes
- [x] Commit: `feat(mcp): add aether.agents.find and aether.people.find`

## Phase 36: Add access requests to listed service agents

- [x] Migration `20261016040000_add_origin_to_invites.exs`
- [x] `create_request/3`, `approve_request/3`
- [x] `snapshot/1`: both sides see `requested` rows; 50-row host view; `parse_poll/1` does not change
- [x] `origin` and `receiverEndpoint` for `requested` rows
- [x] Request row sides: the service is the sender (`from`), the requester is the receiver (`to`)
- [x] One-way contact for a request
- [x] `server/PROTOCOL.md` "Requests"
- [x] Tests: `requests_test.exs` (both sides list a `requested` row)
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server): add access requests to listed service agents`

## Phase 37: Approve, decline or block access requests

- [x] Migration `014_access_requests.sql` (group `requests`, `hub_blocks`)
- [x] `requests` group cannot be deleted
- [x] `apply_grant_inner` adds to `friends` only when no group allows the identity
- [x] `sign_invite` takes `access_group`
- [x] Hub client: `Status::Requested`, `Origin`, `approve`, `block` (`blocks` and `unblock` from Phase 19); `invites()` needs no new parameter
- [x] `host_step` for requests and blocks; a request approval with a group other than `requests` is refused
- [x] Request rows, Approve (days only, group always `requests`), Decline, Block, Blocked list with Unblock
- [x] `.page(hub::approve_request)`, `.page(hub::block_request)`; `/hub/approve` and `/hub/block-request` in `router()`
- [x] Queue text from `node_config::queue_limits`
- [x] `README.md` request flow
- [x] `test_hub`: `requested` rows in every invite list, approve, `PUT /v1/blocks/{endpoint}`
- [x] Tests: eight tests named in the plan, with `a_request_grant_always_joins_the_requests_group`
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): approve, decline or block access requests`

## Phase 38: Confirm access requests on the caller page

- [x] Migration `015_access_request_drafts.sql` (drafts table; listing columns on `hub_approvals`)
- [x] Draft store methods with the limit of 5 and 24 h expiry
- [x] `crates/aether/src/access_requests.rs` `draft`
- [x] Hub client `request_access`
- [x] Caller page section with the IP note, Confirm and Discard; waiting rows "Waiting for {from} to approve" with Cancel
- [x] `confirm_request` refuses an expired listing; `Limit` text at 5 open requests
- [x] `.page(caller::confirm_request)`, `.page(caller::discard_request)`; `/requests/confirm` and `/requests/discard` in `router()`
- [x] `caller_step` verifies the kept listing with `sender_endpoint`; expiry checked at the request time
- [x] Draft badge and host alert
- [x] `README.md`: the user confirms each request
- [x] `test_hub`: `POST /v1/requests` (service is the sender; `limit` at 5)
- [x] Tests: six tests named in the plan, with `a_kept_listing_that_expires_before_delivery_still_accepts_a_bundle_from_its_endpoint`
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): confirm access requests on the caller page`

## Phase 39: Add aether.access.request with local confirmation

- [x] `aether.access.request` makes a draft only
- [x] `access` is `requested` in the find tools
- [x] Instructions sentence
- [x] Tests: four tests named in the plan
- [x] `mise run check` passes
- [x] Commit: `feat(mcp): add aether.access.request with local confirmation`

## Phase 40: Add owner links signed by both keys

- [x] Migration `20261016050000_create_owner_links.exs`
- [x] `AetherHub.OwnerLinks` `claim`, `confirm`, `list`, `delete`, `active_owner`, `delete_expired`
- [x] Listing rows show `owner`
- [x] Rename and deletion remove links; the sweeper removes expired ones
- [x] Four routes; `server/PROTOCOL.md` "Owner links"
- [x] Tests: `owner_links_test.exs`
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `feat(server): add owner links signed by both keys`

## Phase 41: Link a service agent to its owner's personal agent

- [x] Hub client owner link methods
- [x] Owner messages signed with `endpoint.key`
- [x] Personal page "Your service agents" with the two presence sentences; service page "Owner"; `Limit` text
- [x] Pages, routes and `test_hub` routes registered
- [x] `README.md`: owner links and the presence sentences
- [x] Tests: three tests named in the plan
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): link a service agent to its owner's personal agent`

## Phase 42: Describe hub trust, privacy, limits and upgrade

- [x] `server/PROTOCOL.md` intro, "Trust", "Privacy" (with the owner link presence sentence), "Limits", "Changes from the first hub release" (with `GET /v1/hub`, the `x-aether-protocol: 2` header and the 426 `upgrade_required` error)
- [x] `server/README.md` environment, proxy (`TRUSTED_PROXIES` required), FTS5; "Before an upgrade" says to release the new node first
- [x] `server/README.md` "Before an upgrade": stop the hub; copy `aether_hub.db` and its `-wal` and `-shm` files; rollback restores them and deploys the previous image tag, never the new image on the restored file
- [x] `server/lib/aether_hub.ex` `@moduledoc`
- [x] `cd server && mix compile --warnings-as-errors` passes
- [x] `cd server && mix test` passes
- [x] Commit: `docs(server): describe hub trust, privacy, limits and upgrade`

## Verification

- [x] `mise run check` passes (fmt, clippy `-D warnings`, `cargo test --workspace`, build, server tests)
- [x] `cd server && mix test` passes
- [x] Privacy (D1, D6): no MCP server, tool, model, runtime, sandbox or path on any caller-facing surface. Each surface has a passing test:
  - [x] PACT card key allowlist: `the_agent_card_advertises_both_profiles_and_needs_no_token`
  - [x] Iroh card allowlist: `validation_refuses_fields_outside_the_allowlist`, `the_default_card_is_a_valid_a2a_card_with_one_general_skill`
  - [x] `aether.agents.list`: `agents_list_shows_each_card_as_untrusted_owner_text`
  - [x] `aether.agents.find`: `agents_find_marks_listing_text_untrusted_and_removes_control_characters`
  - [x] Listings: `a_listing_with_another_name_or_extra_keys_is_refused` and the allowlist table in `server/test/listings_test.exs`
  - [x] Contacts: "lastSeen is rounded down to 15 minutes" (`server/test/contacts_test.exs`)
  - [x] Tool and plugin texts: `rg -n -i 'codex|folder|sandbox|\breal\b' crates/aether/src/mcp.rs integrations/hermes-plugin` returns nothing
  - [x] Caller page: `rg -n -i 'folder|sandbox' crates/aether/src/admin_ui/caller.rs` returns nothing
- [x] Local approval: a hub row that the page never sent gets no grant (`a_hub_row_the_owner_never_sent_gets_no_grant`); a changed name, days or endpoint gets no grant
- [x] Bundle checks: a bundle with another `agent_id` or `endpoint_id` than `from` and `senderEndpoint` is not kept; a request bundle needs the listing signature of `senderEndpoint`; a listing that expires after the request but before delivery still passes
- [x] Join by endpoint: the find tools show `access: ready` and `contact: true` only when the hub row endpoint equals a local contact endpoint; a local contact with the same full name and another endpoint gives a warning (`find_tools_join_local_contacts_by_endpoint_not_by_name`)
- [x] Signed queries: `GET /v1/agents?q=tax%20law` and `q` with `#`, `&` and non-ASCII text pass the hub signature check
- [x] Names: two keys share a name with different numbers; a released tag never goes to another key; a rename keeps the number when free; `/v1/people/bob/0042` and `/v1/agents/bob/0042` use two segments
- [x] Agent types: the type does not change on the hub or the node, also after an account deletion; personal agents are never listed; service agents are not found by people lookup; an invite by name to a service agent gets the same 404 as an unknown name
- [x] Card: a `v2` line imports with the exact card bytes; a changed card byte is refused; a `v1` line still imports; the longest line is at most 8 KiB with the form overhead
- [x] Search: FTS5 query with operators returns 200; endpoints appear only for contacts; online status of a personal agent appears only for contacts
- [x] Requests: only to listed service agents; no free text; 5 open and 10 a day per requester (409 `limit` shows the `Limit` text, not the name text); a blocked requester sees `requested` while the host sees nothing; grants always go to `requests`, not `friends` (`a_request_grant_always_joins_the_requests_group`)
- [x] Blocks: a personal agent blocks an invite sender by invite id; new invites from it do not show
- [x] MCP: `aether.access.request` sends nothing to the hub until the page confirms
- [x] Delivery: a task to an offline contact returns `status: "waiting"` and the task ID; `node serve` sends it again with the same task ID and idempotency key; the host keeps exactly one task; the answer appears in `aether.inbox.read`; a task ends as `not_delivered` after 7 days or at access end; with a 0.2.8 `node serve` running, `aether.task.send` binds no second endpoint
- [x] Agent tools: a node task gets the agent's `config.toml` servers and not the owner's; elicitations are declined
- [x] Old grants: a `review` grant in the direct scope runs with the general instruction (`a_direct_review_grant_runs_with_the_general_instructions`); a PACT task keeps its Brand capability
- [ ] Hub migrations: run all on a copy of the production database; each old handle keeps its name, gets a number from 1 to 9999, kind `personal`, and a higher version; no two rows share `name` and `number`; open invites and contacts still list (not run: no copy of the production database here; `server/test/migration_test.exs` covers a made-up old database)
- [ ] Hub backup: stop the hub, then copy `/data/aether_hub.db` and, when they exist, `aether_hub.db-wal` and `aether_hub.db-shm` (WAL mode; the runtime image has no `sqlite3`) (an operator step on the server; `server/README.md` "Before an upgrade" has the steps)
- [ ] Hub rollback on a test copy: `down` of the names migration raises; stop the new hub, restore the copied files (delete a `-wal` or `-shm` that the new hub made), deploy the previous image tag, and the old schema and rows are back; the new image on the restored file runs the names migration again, so do not start it; the blocks, listings, origin and owner links migrations roll back with `mix ecto.rollback` (partly done on a temporary database: the names migration `down` raises, and the blocks, listings, origin and owner links migrations roll back and apply again with `mix ecto.rollback`; the Docker restore of an image tag was not run)
- [ ] Node migrations: open a 0.2.8 `node.sqlite` with the new binary; `user_version` equals `NODE_MIGRATIONS.len()`; tasks, grants, groups and contacts stay; a `node.sqlite.backup.<time>` file exists; the `friends` group and its members do not change; group `requests` exists (partly done: `a_0_2_8_database_opens_with_every_migration_and_an_older_binary_refuses_it` opens the 0.2.8 schema and checks the version, the backup file, a contact, `friends` and `requests`; it has no task or grant rows, and no real 0.2.8 file was used)
- [x] Node rollback: an older binary refuses the new schema with `InvalidState`; the backup file restores it; the kept contact bundles are still `v1` JSON that 0.2.8 reads
- [ ] Old clients: a 0.2.8 node gets 426 `upgrade_required` on every `/v1` route except `GET /v1/hub`; its page shows the fixed "The hub cannot be reached now" text; a 0.2.8 caller still sends a task to an existing contact over Iroh; a task from a new caller to an online 0.2.8 host returns `status: "waiting"`, and the outbox sends it only at the next `OFFLINE_PROBE`; a 0.2.8 caller refuses a `v2` line with "This is not an Aether invite" (not run: needs a 0.2.8 binary next to a new hub and node; only the 426 answer has a test)
- [x] Old hub: the old hub ignores the `x-aether-protocol` header; `GET /v1/hub` answers 404 on the old hub; a new node then shows "This hub is older than this node", sends no `{name}` claim, and the find tools say "the hub does not support search"; `aether.people.find` still returns local contacts; a 404 from a find route on the new hub means "not found"
- [ ] Rate limits: behind the proxy, `TRUSTED_PROXIES` is set before the deploy (required: without it, the `search_ip` and `new_account_ip` buckets count all clients as one IP) (a deploy step: read the subnet of the Coolify proxy network on the server)
- [ ] Manual: two nodes through `https://aether-relay.tudorandrei.xyz` and the public hub; invite by full name, task answered; service agent listed, found by `aether.agents.find`, request confirmed on the page, approved on the host page, task answered; the `RELAY-TEST.md` outbox check (not run: needs two computers with Codex logins, the public relay and the public hub)
- [x] Live: `AETHER_RUN_LIVE_CODEX=1 cargo test -p aether live_app_server_lists_the_agent_mcp_servers -- --ignored`
- [ ] PACT: run PACT's own conformance suite (`e2e/` in the PACT repository, `E2E_PROVIDER=any`) against the card without provider; expect the same 7 of 10 Identity tests as in `README.md` (not run: the suite is in the PACT repository)

## Review

- [ ] Code reviewed (no review by another person or agent: the user asked for no subagents)
- [x] PLAN.md updated if the approach changed during implementation
- [x] All phase commits are clean and describe their intent
- [x] No tombstone tests (only current contracts are tested)
- [ ] Open questions answered or moved to a later plan (the questions in PLAN.md stay open for the user)
- [ ] TODO.md items all checked off (the manual, deploy and old-binary checks above stay open)
