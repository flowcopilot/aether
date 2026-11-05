defmodule AetherHub.Versions do
  @moduledoc """
  The version counter of each account. A write bumps the versions of the
  endpoints that it touched in its transaction, and wakes their long polls
  after the commit.
  """

  import Ecto.Query

  alias AetherHub.{Account, Notify, Repo}

  def current(endpoint),
    do: Repo.one(from a in Account, where: a.endpoint == ^endpoint, select: a.version)

  def bump([]), do: :ok

  def bump(endpoints) do
    Repo.update_all(from(a in Account, where: a.endpoint in ^Enum.uniq(endpoints)),
      inc: [version: 1]
    )

    :ok
  end

  @doc """
  Wakes the endpoints of a transaction result `{:ok, {value, touched}}` and
  returns `{:ok, value}`. An error passes through.
  """
  def notify({:ok, {value, touched}}) do
    Notify.changed(touched)
    {:ok, value}
  end

  def notify({:error, _} = error), do: error
end
