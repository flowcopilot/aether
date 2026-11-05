defmodule AetherHub.People do
  @moduledoc """
  Finds personal agents by name. An exact lookup finds every personal agent;
  a search lists only the ones whose owner opted in. Neither one tells the
  person that someone looked.

  A row has `online` and `endpoint` only when the person is a contact of
  the node that asks.
  """

  import Ecto.Query

  alias AetherHub.{Account, Contact, Names, Presence, Repo}

  @query ~r/\A[a-z0-9._-]{2,32}\z/
  @default_limit 10
  @max_limit 20
  # Every character of the name alphabet sorts before "{", so the names that
  # start with `q` are the range from `q` up to `q <> "{"`.
  @after_alphabet "{"

  @doc """
  The personal agent `name#number`, as seen by `asker` at Unix time `now`.
  `{:error, :not_found}` for an unknown name or a service agent.
  """
  def lookup(asker, name, number, now) do
    with {:ok, _} <- fetch_account(asker) do
      case Repo.one(
             from a in Account,
               where: a.name == ^name and a.number == ^number and a.kind == "personal"
           ) do
        nil -> {:error, :not_found}
        account -> {:ok, row(account, contacts(asker, [account]), now)}
      end
    end
  end

  @doc """
  The `q` and `limit` of a search request: `q` has 2 to 32 characters of the
  name alphabet, and `limit` is 1 to 20 (default 10).
  """
  def parse_search(params) do
    with q when is_binary(q) <- params["q"],
         true <- Regex.match?(@query, q),
         {:ok, limit} <- parse_limit(params["limit"]) do
      {:ok, q, limit}
    else
      _ -> {:error, :invalid}
    end
  end

  defp parse_limit(nil), do: {:ok, @default_limit}

  defp parse_limit(value) when is_binary(value) do
    case Integer.parse(value) do
      {limit, ""} when limit in 1..@max_limit -> {:ok, limit}
      _ -> :error
    end
  end

  defp parse_limit(_value), do: :error

  @doc """
  The personal agents that opted in to search and whose name starts with
  `q`, ordered by name and number, as seen by `asker` at Unix time `now`.
  """
  def search(asker, q, limit, now) do
    with {:ok, _} <- fetch_account(asker) do
      accounts =
        from(a in Account,
          where:
            a.name >= ^q and a.name < ^(q <> @after_alphabet) and a.kind == "personal" and
              a.searchable == true,
          order_by: [asc: a.name, asc: a.number],
          limit: ^limit
        )
        |> Repo.all()

      contacts = contacts(asker, accounts)
      {:ok, %{"people" => Enum.map(accounts, &row(&1, contacts, now))}}
    end
  end

  # The endpoints in `accounts` that are contacts of `asker`.
  defp contacts(asker, accounts) do
    endpoints = Enum.map(accounts, & &1.endpoint)

    from(c in Contact,
      where: c.owner_endpoint == ^asker and c.contact_endpoint in ^endpoints,
      select: c.contact_endpoint
    )
    |> Repo.all()
    |> MapSet.new()
  end

  defp row(%Account{endpoint: endpoint} = account, contacts, now) do
    handle = Names.format(account.name, account.number)

    if MapSet.member?(contacts, endpoint) do
      %{
        "handle" => handle,
        "contact" => true,
        "online" => Presence.online?(endpoint, now),
        "endpoint" => endpoint
      }
    else
      %{"handle" => handle, "contact" => false}
    end
  end

  defp fetch_account(endpoint) do
    case Repo.get(Account, endpoint) do
      nil -> {:error, :no_account}
      account -> {:ok, account}
    end
  end
end
