defmodule AetherHub.Versions do
  @moduledoc """
  The version counter of each account. A write that changes what an
  endpoint sees runs through `commit/1`: it bumps the versions of the
  endpoints that it touched in its transaction, and wakes their long polls
  after the commit.
  """

  import Ecto.Query

  alias AetherHub.{Account, Notify, ReadRepo, Repo}

  # Endpoints in one `IN` list. SQLite refuses a statement with more than
  # 32,766 variables (999 before 3.32), so a bump goes in bounded chunks.
  @bump_chunk 500

  @doc """
  The version of `endpoint`, from the read pool. Inside a write transaction,
  read it with `Repo` instead.
  """
  def current(endpoint),
    do: ReadRepo.one(from a in Account, where: a.endpoint == ^endpoint, select: a.version)

  @doc """
  Runs `fun` in a transaction. `fun` returns `{value, touched}`: the
  versions of the endpoints in `touched` go up in the same transaction, and
  their long polls wake after the commit. `{:ok, value}`, or `:ok` when
  `value` is `:ok`; `{:error, reason}` after `Repo.rollback(reason)`.
  """
  def commit(fun) do
    Repo.atomic(fn ->
      {value, touched} = fun.()
      bump(touched)
      {value, touched}
    end)
    |> case do
      {:ok, {value, touched}} ->
        Notify.changed(touched)
        if value == :ok, do: :ok, else: {:ok, value}

      {:error, _} = error ->
        error
    end
  end

  defp bump(endpoints) do
    endpoints
    |> Enum.uniq()
    |> Enum.chunk_every(@bump_chunk)
    |> Enum.each(fn chunk ->
      Repo.update_all(from(a in Account, where: a.endpoint in ^chunk), inc: [version: 1])
    end)
  end
end
