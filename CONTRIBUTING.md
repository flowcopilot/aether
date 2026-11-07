# Contributing to Aether

Thank you for helping. Bug reports, documentation fixes, and code are all
welcome. To report a vulnerability, follow [SECURITY.md](SECURITY.md)
instead of opening an issue.

## Before you start

For a bug, open an issue with the steps to reproduce it. For a larger
change, open an issue first so that we can agree on the approach before you
write the code. Changes to the wire protocol, the invite format, the hub
protocol (`server/PROTOCOL.md`), or a database migration need that
discussion, because released nodes depend on them.

## Set up

The repository pins its tools with [mise](https://mise.jdx.dev/):

```sh
mise install
mise run check
```

`mise run check` runs formatting, Clippy, the Rust tests, the hub tests, the
Hermes plugin tests, and the TLA+ models. See
[docs/development.md](docs/development.md) for the other checks: product
features, two nodes on one machine, container proofs, and live Codex tests.

## Make a change

- Keep a pull request to one change. Add or update tests for the behavior
  that you change. Product behavior is described in `features/` as Gherkin;
  update the feature when the behavior changes.
- Rust code must pass `cargo fmt` and the workspace Clippy lints with no
  warnings. Production code must not panic; see `Cargo.toml`.
- Write documentation in short, plain sentences.

## Commit messages

Commits follow [Conventional Commits](https://www.conventionalcommits.org/).
The release workflow reads them to pick the next version and write the
changelog, and pull requests fail when a commit does not follow them.

```text
feat(hub): search listings by tag
fix(node): keep the outbox when the relay is down
docs: explain access groups
```

Use `feat` for a new capability, `fix` for a bug fix, and `docs`, `test`,
`refactor`, `style`, `perf`, `build`, `ci`, or `chore` for the rest. Mark a
breaking change with `!` after the type or a `BREAKING CHANGE:` footer. To
check your commits locally, install [Cocogitto](https://docs.cocogitto.io/)
and run `cog check`.

## Open a pull request

Fill in the [pull request template](.github/pull_request_template.md) and
delete the sections that do not apply. List the checks that you ran and
their results, and say what stays unchecked. CI builds the release binaries
for pull requests that change Rust or packaging files; it does not run the
test suite, so run it locally.

By contributing, you agree that your contribution is licensed under the
[Apache License 2.0](LICENSE).
