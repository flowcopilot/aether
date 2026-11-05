defmodule AetherHub.Blocks do
  @moduledoc """
  Blocks. A blocked node gets the same answers as before: its invites to the
  owner are kept for it alone, and the owner never sees them. A block stays
  when either account is deleted.
  """

  import Ecto.Query

  alias AetherHub.{Account, Block, Names, Repo}

  @max 1_000

  @doc "Whether `owner` blocked `other`."
  def blocked?(owner, other) do
    Repo.exists?(
      from b in Block, where: b.owner_endpoint == ^owner and b.blocked_endpoint == ^other
    )
  end

  @doc "Blocks `blocked` for `owner`. `{:error, :limit}` at #{@max} blocks."
  def put(owner, blocked) do
    count = Repo.aggregate(from(b in Block, where: b.owner_endpoint == ^owner), :count)

    cond do
      blocked?(owner, blocked) ->
        :ok

      count >= @max ->
        {:error, :limit}

      true ->
        Repo.insert_all(Block, [
          %{
            owner_endpoint: owner,
            blocked_endpoint: blocked,
            created_at: System.system_time(:second)
          }
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
      select: {b.blocked_endpoint, a.name, a.number}
    )
    |> Repo.all()
    |> Enum.map(fn
      {endpoint, nil, _} ->
        %{"endpoint" => endpoint, "handle" => nil}

      {endpoint, name, number} ->
        %{"endpoint" => endpoint, "handle" => Names.format(name, number)}
    end)
  end
end
