defmodule AetherHub.Directory do
  @moduledoc """
  The public view of the hub for the landing page. It needs no account.

  It shows only what every node can already see: the listed service agents,
  their listing text, whether they are online, and the full name of the
  owner when an owner link is active. A row has no endpoint and no
  signature. For people it gives only a count: no name and no presence,
  because only contacts see the presence of a person.

  Building the view reads every live listing, so the route serves it from
  `AetherHub.DirectoryCache`, which builds it once in 15 seconds at most.
  """

  import Ecto.Query

  alias AetherHub.{Account, Clock, DirectoryCache, Listing, Listings, Names, OwnerLinks}
  alias AetherHub.{Presence, Repo}

  @max_agents AetherHub.Protocol.limit(:max_directory_agents)
  @personal AetherHub.Protocol.kind(:personal)

  @doc """
  The directory as the route serves it: the cached view, at most 15 seconds
  old, with its age in whole seconds. `{:error, :unavailable}` when the cache
  cannot answer. See `AetherHub.DirectoryCache`.
  """
  def cached do
    {view, age_ms} = DirectoryCache.fetch()
    {:ok, view, div(age_ms, 1000)}
  rescue
    DirectoryCache.Unavailable -> {:error, :unavailable}
  end

  @doc """
  Builds the directory now: the listed service agents, online agents first,
  then by the latest update, at most #{@max_agents}. Also the counts of
  listed agents, of online listed agents, and of personal accounts.

  It reads only the cheap columns of every live listing, and decodes the
  listing text of the agents on the page. The hub checked each listing when
  the node sent it, so the counts do not decode. A listing that does not
  read is a bad row: the page leaves it out and the next agent takes its
  place, but `listed` and `online` still count it.
  """
  def snapshot do
    rows =
      from(l in Listing,
        join: a in Account,
        on: a.endpoint == l.endpoint,
        where: l.expires_at > ^Clock.now(),
        order_by: [desc: l.updated_at, asc: l.id],
        select: {l.id, l.endpoint, a.name, a.number}
      )
      |> Repo.all()
      |> Enum.map(fn {id, endpoint, name, number} ->
        {id, endpoint, Names.format(name, number), Presence.online?(endpoint)}
      end)

    # Online agents first, each group by the latest update.
    {online, offline} = Enum.split_with(rows, fn {_id, _endpoint, _handle, online} -> online end)

    agents = (online ++ offline) |> cards() |> Enum.take(@max_agents)

    # The full name of the owner only: never the owner's presence or
    # endpoint.
    owners = OwnerLinks.active_owners(Enum.map(agents, fn {endpoint, _agent} -> endpoint end))

    %{
      "agents" =>
        Enum.map(agents, fn {endpoint, agent} -> Map.put(agent, "owner", owners[endpoint]) end),
      "listed" => length(rows),
      "online" => length(online),
      "people" => Repo.aggregate(from(a in Account, where: a.kind == ^@personal), :count)
    }
  end

  # The agents of the rows in pages of the directory size. A page reads its
  # listing texts in one query, and the next page is read only when bad rows
  # left the page short.
  defp cards(rows) do
    rows
    |> Stream.chunk_every(@max_agents)
    |> Stream.flat_map(fn page ->
      ids = Enum.map(page, fn {id, _endpoint, _handle, _online} -> id end)
      texts = Map.new(Repo.all(from l in Listing, where: l.id in ^ids, select: {l.id, l.listing}))

      Enum.flat_map(page, fn {id, endpoint, handle, online} ->
        with text when is_binary(text) <- texts[id],
             {:ok, card} <- Listings.card(text) do
          [{endpoint, Map.merge(card, %{"handle" => handle, "online" => online})}]
        else
          _ -> []
        end
      end)
    end)
  end
end
