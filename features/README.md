# Features

This directory is the list of what Aether does. Each `.feature` file
describes one product feature in Gherkin. Each scenario is also a test.

| Directory | Part of the product      | Step definitions                    | Runner                        |
| --------- | ------------------------ | ----------------------------------- | ----------------------------- |
| `node/`   | The `aether` node (Rust) | `crates/aether/tests/steps/`        | [cucumber-rs]                 |
| `hub/`    | The hub server (Elixir)  | `server/test/features/*_test.exs`   | `server/test/support/feature.ex`  |

## Run

```sh
mise run features                                         # all features
cargo test -p aether --test features                      # node only
cargo test -p aether --test features -- --name "cancel"   # scenarios whose name matches
cd server && mix test test/features                       # hub only
cd server && mix test test/features/invites_test.exs      # one hub feature
```

## Write a scenario

- Write the scenario for a reader who knows the product, not the code.
  Name the behaviour, not the function: "A revoked member cannot send new
  work", not "admit returns Denied".
- One scenario shows one rule. Use `Scenario Outline` with `Examples` when
  the same rule holds for several values.
- Use `Background` for the setup that all scenarios of a file share.
- A step without a definition fails the run. It does not pass in silence.
- Name files in kebab case, for example `access-requests.feature`.
- Tag a scenario `@unix` when it needs unix APIs. Other platforms skip it.

## Node steps (Rust)

Steps share one `World` for each scenario (`steps/mod.rs`). Each area keeps
its own state in its own module. Cucumber runs the scenarios concurrently on
one task, so a step that blocks must use `tokio::task::spawn_blocking`. Tag a
scenario `@serial` when it must run alone.

## Hub steps (Elixir)

A test module reads one feature file:

```elixir
defmodule AetherHub.Features.InvitesTest do
  use AetherHub.Feature, file: "invites.feature", async: true

  defstep ~r/^(?<name>\w+) invites (?<other>\w+)$/, %{name: name, other: other}, state do
    {:ok, %{invite: invite(state[name], state[other])}}
  end
end
```

A step matches the text after the keyword. It gets the named captures, the
data table as `:table` (a list of maps) and the doc string as `:doc_string`.
A step that returns `{:ok, map}` merges the map into the scenario state.
Scenario tags become ExUnit tags. A capture cannot use an Elixir reserved
word (such as `after` or `end`) as its name.

[cucumber-rs]: https://github.com/cucumber-rs/cucumber
