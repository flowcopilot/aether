<!--
Write for a reviewer who will not open the files. Short prose.
Delete any section that does not apply.
-->

## Why

<!-- One or two sentences. Name the related issue if there is one. -->

## What changes

<!--
One picture, with a Before and an After. Draw two diagrams, Before and
After, or draw one diagram and mark the changed parts with
`classDef changed`. For a new thing, write "Before: none."

Pick the diagram with one question: does the next step depend on an input
from outside, which can come late, come wrong, or not come?
- Yes: a Mermaid state diagram (stateDiagram-v2). An outside input is a
  user action, a reply from a service or a device, a webhook, a timer, or
  a cancel. A state is a place where the thing waits, for example a sent
  task in "Waiting" for the other node, or a task in "Pending approval".
  An arrow is the event that moves it on. Draw the arrows for the bad
  events too (timeout, error, cancel, retry), because the change can go
  wrong there.
- No: a Mermaid flowchart. The procedure runs from start to end with the
  data that it has, for example a node that loads its policy at start, a
  CLI command, or a hub migration. Boxes are steps, arrows mean "then",
  and diamonds are decisions on that data.
- No diagram fits (a protocol version bump, a rename, a setting value): a
  table of old and new values, a diff, or pseudocode.
Add up to three bullets for what the picture misses, including what you
left out on purpose.
-->

**Look at:** <!-- the one file or decision that needs a careful look -->

## Evidence

<!--
This repository has no CI. List each check that you ran and its result,
then list what stays unchecked and why (for example, Docker or Elixir is
not available). Run the checks for the parts that you changed:
- Rust: `mise run fmt`, `mise run lint`, and `mise run test`, or
  `mise run check` for all local checks.
- Hub (server/): `mise run server-test`.
- Hermes plugin: `mise run hermes-plugin-test`. Say if the adapter tests
  skipped because Hermes was not on PYTHONPATH.
- Product features (features/): `mise run features`.
- Node to node: `cargo test -p aether --test features -- --tags @node-to-node`, the two-node run in
  LOCAL-TEST.md, or `mise run container-proof` for the direct and relay
  paths. Tests with the real Codex binary run only with
  `AETHER_RUN_LIVE_CODEX=1`.
- Promo film (promo/signal): `npm run lint`, `npm run build`, and a still
  or `mise run film` for a visual change.

For test changes, state the distinct regression protected or surviving
coverage after removal, and material runtime cost changes with costs
marked as measured or estimated.

A visible change to the node web UI or the hub landing page shows before
and after, side by side: screenshots for layout and styling, and a short
recording or GIF for motion. Crop to the changed area. When a "before"
cannot be captured, say why. Attach files with `gh pr create --attach`.
Do not commit them.
-->

## Merge danger

**Door:** <!-- one-way (hard to undo: a migration in crates/aether-store/migrations or server/priv/repo/migrations, a protocol, invite, or key format that released nodes read, a hub deploy) or two-way (a revert fixes it) -->
**Blast radius:** <!-- one word, e.g. none, promo, hub, relay, one node, all nodes -->

<!--
One or two sentences on what could break and for whom. Answer these when
they apply:
- Can a node from this branch still talk to released nodes and to the
  hub? Name changes to A2A frames, the invite line (`aether-invitation/v2`),
  the Agent Card, or the hub protocol (server/PROTOCOL.md). The hub refuses
  old nodes with `426 upgrade_required`.
- Does the change touch policy, grants, access groups, signing keys, or
  relay or hub settings? Does an owner need a node restart for it to take
  effect? Policy changes need a restart.
-->

## After merge

<!--
Manual steps in order. Delete if none. For example:
1. Publish the node release for all supported platforms (see
   packaging/README.md) before a hub deploy that refuses older nodes, and
   tell users to update.
2. Back up the hub database, then deploy the hub. Follow "Before an
   upgrade" in server/README.md.
3. Rebuild and deploy the relay image (Dockerfile.relay,
   compose.coolify.yml).
4. Owners update with their installation method (see docs/installation.md)
   and restart `aether node serve`.
-->
