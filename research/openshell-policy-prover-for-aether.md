# OpenShell policy prover for Aether

Checked on 2026-09-18. This assessment uses the NVIDIA article and the current `main` documentation. The upstream code can change.

The prover can help Aether check policy changes. It cannot prove that an arbitrary agent task is safe. Direct reuse requires OpenShell policy inputs. Aether needs its own formal model for its request and grant rules.

## What the prover checks

The main containment question is whether any action is permitted by a candidate policy but prohibited by an operator boundary. Z3 searches for such an action. A counterexample means that the candidate exceeds the boundary. No counterexample means containment only within the formal model. An unknown result does not grant permission. [NVIDIA article](https://nvidia.github.io/OpenShell-Research/dev-notes/posts/2026-09-10-learning-formal-methods-agent-policy-prover/)

The current implementation has two interfaces with different purposes:

- The gateway uses proposal-risk checks. These report new link-local access, credential access through a protocol that bypasses HTTP inspection, new credentialed endpoint access, and new HTTP methods on existing credentialed endpoints.
- The independent containment API checks a complete candidate policy against a boundary. The current documentation says that the gateway still uses the proposal-risk API until its managed-policy migration. An empty list of risk findings is therefore not a containment proof. [Prover source documentation](https://github.com/NVIDIA/OpenShell/blob/main/crates/openshell-prover/README.md)

The standalone command is suitable for a local experiment:

```sh
openshell-prover check candidate.yaml --boundary boundary.yaml --output json
```

Only exit code `0` means containment succeeded. The command does not fetch, compose, approve, or apply policies. A caller must supply all effective permissions, including provider rules. The default solver budget is 10 seconds. [CLI reference](https://github.com/NVIDIA/OpenShell/blob/main/docs/reference/policy-prover.mdx)

## Limits that matter

The standalone model currently covers filesystem access, L4 network access, and enforced REST methods and paths, including REST deny rules. Unsupported policy controls produce a failure. The article describes a broader runtime model and links to a research branch. Use the scope in the current API evidence for an integration decision. [Article](https://nvidia.github.io/OpenShell-Research/dev-notes/posts/2026-09-10-learning-formal-methods-agent-policy-prover/) and [CLI reference](https://github.com/NVIDIA/OpenShell/blob/main/docs/reference/policy-prover.mdx)

The model does not prove task intent, least privilege, runtime enforcement, or task results. The proposal-risk model is per sandbox. Its credential input records host-level credential presence, without credential scope modeling. [Prover source documentation](https://github.com/NVIDIA/OpenShell/blob/main/crates/openshell-prover/README.md)

Some ordinary policy comparisons remain unsupported. Examples include overlapping L4 and enforced REST endpoints, unresolved image paths, and most containment comparisons between different filesystem paths. Symlinks can invalidate a simple path-prefix comparison. Network selector literals must use ASCII. Solver timeouts and resource limits produce inconclusive results. [CLI reference](https://github.com/NVIDIA/OpenShell/blob/main/docs/reference/policy-prover.mdx)

This is more than article example code. NVIDIA documents a standalone binary, JSON output, typed Rust evidence, tests, and release artifacts. However, the Rust crate is not a stable published SDK. The reviewed crate and CLI documentation declare Apache-2.0. Check the license files and pin a source revision before reuse. No local build or runtime benchmark was done for this assessment. [Prover source documentation](https://github.com/NVIDIA/OpenShell/blob/main/crates/openshell-prover/README.md), [CLI package](https://github.com/NVIDIA/OpenShell/blob/main/crates/openshell-prover-cli/README.md), and [CLI reference](https://github.com/NVIDIA/OpenShell/blob/main/docs/reference/policy-prover.mdx)

## Proposed use in Aether

These are design recommendations, not existing integration claims:

1. Keep the current request checks. Checking one structured request against an exact grant is a direct permission check. It does not need an SMT solver.
2. Use a solver when Aether needs to prove that a new policy or delegated grant permits no request outside an approved boundary. Model Aether identities, operations, resources, constraints, and delegation rules explicitly.
3. If Aether uses OpenShell to execute tasks, check the complete proposed sandbox policy with the standalone prover. This proves only the supported sandbox policy properties. It does not replace Aether admission checks.
4. Run containment checks when policies or grants change. Record the candidate, boundary, model version, result, and counterexample. Bind each result to the exact policy revision. Do not reuse it after a relevant change.
5. Reject unsupported and inconclusive results, or send them for review. Measure solver cost before adding any solver call to request admission.

Aether's current middleware receives a structured request. The task text reaches execution through a separate field. Thus, adding this prover to middleware cannot prove the safety of the text or of later tool actions. See [PolicyMiddleware and PolicyEngine](../crates/aether-core/src/lib.rs) and [task execution](../crates/aether/src/execution.rs).
