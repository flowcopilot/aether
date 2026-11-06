defmodule AetherHub.People do
  @moduledoc """
  Finds personal agents by name. An exact lookup finds every personal agent;
  a search lists only the ones whose owner opted in. Neither one tells the
  person that someone looked.

  A row has `online` and `endpoint` only when the person is a contact of
  the node that asks.
  """

  import Ecto.Query

  alias AetherHub.{Account, Contacts, Params, Presence, Protocol, Repo}

  # Every character of the name alphabet sorts before "{", so the names that
  # start with `q` are the range from `q` up to `q <> "{"`.
  @after_alphabet "{"
  @personal Protocol.kind(:personal)

  @doc """
  The personal agent `name#number`, as seen by `asker`. `{:error,
  :not_found}` for an unknown name or a service agent.
  """
  def lookup(%Account{endpoint: asker}, name, number) do
    case Repo.one(
           from a in Account,
             where: a.name == ^name and a.number == ^number and a.kind == ^@personal
         ) do
      nil -> {:error, :not_found}
      account -> {:ok, row(account, Contacts.known(asker, [account.endpoint]))}
    end
  end

  @doc "The `q` and `limit` of a search request."
  def parse_search(params) do
    with q when is_binary(q) <- params["q"],
         true <- Regex.match?(Protocol.regex(:people_query), q),
         {:ok, limit} <- Params.limit(params["limit"]) do
      {:ok, q, limit}
    else
      _ -> {:error, :invalid}
    end
  end

  @doc """
  The personal agents that opted in to search and whose name starts with
  `q`, ordered by name and number, as seen by `asker`.
  """
  def search(%Account{endpoint: asker}, q, limit) do
    accounts =
      from(a in Account,
        where:
          a.name >= ^q and a.name < ^(q <> @after_alphabet) and a.kind == ^@personal and
            a.searchable == true,
        order_by: [asc: a.name, asc: a.number],
        limit: ^limit
      )
      |> Repo.all()

    contacts = Contacts.known(asker, Enum.map(accounts, & &1.endpoint))
    {:ok, %{"people" => Enum.map(accounts, &row(&1, contacts))}}
  end

  defp row(%Account{endpoint: endpoint} = account, contacts) do
    handle = Account.full_name(account)

    if MapSet.member?(contacts, endpoint) do
      %{
        "handle" => handle,
        "contact" => true,
        "online" => Presence.online?(endpoint),
        "endpoint" => endpoint
      }
    else
      %{"handle" => handle, "contact" => false}
    end
  end
end
