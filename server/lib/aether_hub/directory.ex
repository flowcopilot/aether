defmodule AetherHub.Directory do
  @moduledoc """
  The public view of the hub for the landing page. It needs no account.

  It shows only what every node can already see: the listed service agents,
  their listing text, whether they are online, and the full name of the
  owner when an owner link is active. A row has no endpoint and no
  signature. For people it gives only a count: no name and no presence,
  because only contacts see the presence of a person.
  """

  import Ecto.Query

  alias AetherHub.{Account, Clock, Listing, Listings, OwnerLinks, Presence, Repo}

  @max_agents AetherHub.Protocol.limit(:max_directory_agents)

  @doc """
  The listed service agents, online agents first, then by the latest
  update, at most #{@max_agents}. Also the counts of listed agents, of
  online listed agents, and of personal accounts.
  """
  def snapshot do
    rows =
      Repo.all(
        from l in Listing,
          join: a in Account,
          on: a.endpoint == l.endpoint,
          where: l.expires_at > ^Clock.now(),
          order_by: [desc: l.updated_at],
          select: {l.endpoint, l.listing, a}
      )

    # The hub checked a listing when the node sent it, so a listing that
    # does not read is a bad row: the page leaves it out.
    agents =
      rows
      |> Enum.flat_map(fn {endpoint, text, account} ->
        case Listings.card(text) do
          {:ok, card} ->
            online = Presence.online?(endpoint)

            [
              {endpoint,
               Map.merge(card, %{"handle" => Account.full_name(account), "online" => online})}
            ]

          :error ->
            []
        end
      end)
      |> Enum.sort_by(fn {_endpoint, agent} -> not agent["online"] end)

    page =
      agents
      |> Enum.take(@max_agents)
      # The full name of the owner only: never the owner's presence or
      # endpoint.
      |> Enum.map(fn {endpoint, agent} ->
        Map.put(agent, "owner", OwnerLinks.active_owner(endpoint))
      end)

    %{
      "agents" => page,
      "listed" => length(agents),
      "online" => Enum.count(agents, fn {_endpoint, agent} -> agent["online"] end),
      "people" => Repo.aggregate(from(a in Account, where: a.kind == "personal"), :count)
    }
  end
end
