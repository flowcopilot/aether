defmodule AetherHub.Versions do
  @moduledoc """
  The version counter of each account. A write that changes what an
  endpoint sees runs through `commit/1`: it bumps the versions of the
  endpoints that it touched in its transaction, and wakes their long polls
  after the commit.
  """

  import Ecto.Query

  alias AetherHub.{Account, Notify, Repo}

  def current(endpoint),
    do: Repo.one(from a in Account, where: a.endpoint == ^endpoint, select: a.version)

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

  defp bump([]), do: :ok

  defp bump(endpoints) do
    Repo.update_all(from(a in Account, where: a.endpoint in ^Enum.uniq(endpoints)),
      inc: [version: 1]
    )

    :ok
  end
end
