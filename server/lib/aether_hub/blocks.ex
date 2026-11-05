defmodule AetherHub.Blocks do
  @moduledoc """
  Blocks, and what they hide. A blocked node gets the same answers as
  before: a row that it makes towards the node that blocked it is kept with
  `hidden_from` set to that node, so it exists for the blocked node alone.
  A block stays when either account is deleted. `AetherHub.Accounts.block/2`
  removes what the two nodes shared.
  """

  import Ecto.Query

  alias AetherHub.{Account, Block, Clock, Repo}

  @max AetherHub.Protocol.limit(:max_blocks)

  def blocked?(owner, other) do
    Repo.exists?(
      from b in Block, where: b.owner_endpoint == ^owner and b.blocked_endpoint == ^other
    )
  end

  @doc """
  The `hidden_from` value of a new row that `maker` makes towards `target`:
  `target` when it blocked `maker`, else `nil`.
  """
  def hidden_from(target, maker), do: if(blocked?(target, maker), do: target)

  @doc "Whether `endpoint` sees `row`."
  def visible?(%{hidden_from: hidden_from}, endpoint), do: hidden_from != endpoint

  @doc "The query condition for the rows that `endpoint` sees."
  def visible_to(endpoint),
    do: dynamic([r], is_nil(r.hidden_from) or r.hidden_from != ^endpoint)

  @doc "Blocks `blocked` for `owner`. `{:error, :limit}` at #{@max} blocks."
  def put(owner, blocked) do
    cond do
      blocked?(owner, blocked) ->
        :ok

      Repo.aggregate(from(b in Block, where: b.owner_endpoint == ^owner), :count) >= @max ->
        {:error, :limit}

      true ->
        Repo.insert_all(Block, [
          %{owner_endpoint: owner, blocked_endpoint: blocked, created_at: Clock.now()}
        ])

        :ok
    end
  end

  @doc "Removes a block; `{:error, :not_found}` when there is none."
  def delete(owner, blocked) do
    case Repo.delete_all(
           from b in Block, where: b.owner_endpoint == ^owner and b.blocked_endpoint == ^blocked
         ) do
      {0, _} -> {:error, :not_found}
      _ -> :ok
    end
  end

  @doc "The blocks of `owner`, with the current full name of each, or `nil`."
  def list(owner) do
    from(b in Block,
      left_join: a in Account,
      on: a.endpoint == b.blocked_endpoint,
      where: b.owner_endpoint == ^owner,
      order_by: [asc: b.created_at, asc: b.blocked_endpoint],
      select: {b.blocked_endpoint, a}
    )
    |> Repo.all()
    |> Enum.map(fn {endpoint, account} ->
      %{"endpoint" => endpoint, "handle" => account && Account.full_name(account)}
    end)
  end
end
