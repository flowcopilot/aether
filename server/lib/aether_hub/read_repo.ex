defmodule AetherHub.ReadRepo do
  @moduledoc """
  A read-only pool on the same SQLite file as `AetherHub.Repo`. In WAL mode,
  readers do not block the writer or each other, so reads run in parallel
  while `AetherHub.Repo` keeps its one connection for writes.

  Use it only for a read that decides nothing about a write: a read that a
  write depends on stays on `AetherHub.Repo`, in the transaction of the
  write. A read here sees what was committed when its transaction started,
  so a read after `AetherHub.Repo` returned from a commit sees that commit.

  The repo does not run migrations and is not in `:ecto_repos`. It starts
  after `Ecto.Migrator`, because a read-only connection cannot create the
  file.
  """
  use Ecto.Repo,
    otp_app: :aether_hub,
    adapter: Ecto.Adapters.SQLite3,
    read_only: true

  @doc """
  Runs `fun` in one deferred read transaction, so every query in it sees
  the same committed state: `{:ok, value}`.
  """
  def snapshot(fun), do: transact(fn -> {:ok, fun.()} end)
end
