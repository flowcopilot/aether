defmodule AetherHub.DirectoryViewTest do
  @moduledoc """
  The directory is built from a few queries, and the route serves it from
  the cache. The first tests compare the view with the one that the hub built
  before: one query for the owner of each agent, and every listing decoded.
  They run alone, because they read the whole table of listings.
  """
  use ExUnit.Case, async: false

  import Ecto.Query
  import AetherHub.TestClient

  alias AetherHub.{Account, Clock, Directory, DirectoryCache, Listing, Listings, OwnerLinks}
  alias AetherHub.{Presence, Repo, Seed}

  @max_agents AetherHub.Contract.get(["limits", "maxDirectoryAgents"])

  # The directory as it was before the cache: all the rows, decoded, and the
  # owner of each agent on the page by its own query.
  defp reference do
    rows =
      Repo.all(
        from l in Listing,
          join: a in Account,
          on: a.endpoint == l.endpoint,
          where: l.expires_at > ^Clock.now(),
          order_by: [desc: l.updated_at, asc: l.id],
          select: {l.endpoint, l.listing, a}
      )

    agents =
      rows
      |> Enum.flat_map(fn {endpoint, text, account} ->
        case Listings.card(text) do
          {:ok, card} ->
            [
              {endpoint,
               Map.merge(card, %{
                 "handle" => Account.full_name(account),
                 "online" => Presence.online?(endpoint)
               })}
            ]

          :error ->
            []
        end
      end)
      |> Enum.sort_by(fn {_endpoint, agent} -> not agent["online"] end)

    %{
      "agents" =>
        agents
        |> Enum.take(@max_agents)
        |> Enum.map(fn {endpoint, agent} ->
          Map.put(agent, "owner", OwnerLinks.active_owner(endpoint))
        end),
      "listed" => length(agents),
      "online" => Enum.count(agents, fn {_endpoint, agent} -> agent["online"] end),
      "people" => Repo.aggregate(from(a in Account, where: a.kind == "personal"), :count)
    }
  end

  defp queries(fun) do
    test = self()
    id = {__MODULE__, make_ref()}

    :telemetry.attach(
      id,
      [:aether_hub, :repo, :query],
      fn _event, _measure, _meta, _config -> if self() == test, do: send(test, {:query, id}) end,
      nil
    )

    try do
      result = fun.()
      {result, count_queries(id, 0)}
    after
      :telemetry.detach(id)
    end
  end

  defp count_queries(id, n) do
    receive do
      {:query, ^id} -> count_queries(id, n + 1)
    after
      0 -> n
    end
  end

  setup do
    now = Clock.now()

    # More agents than a page, in two ages, so that ties in the update time
    # and the online agents' place in the order both matter.
    for i <- 1..(@max_agents + 30) do
      Seed.service(
        description: "Agent number #{i}",
        updated_at: now - rem(i, 7),
        online: rem(i, 9) == 0,
        owner: Enum.at([nil, :active, :claim, :expired], rem(i, 4))
      )
    end

    :ok
  end

  test "the view is the one that the hub built before" do
    view = Directory.snapshot()

    assert view == reference()
    assert length(view["agents"]) == @max_agents
    assert view["listed"] >= @max_agents + 30
  end

  test "the online agents come first, then the latest update" do
    agents = Directory.snapshot()["agents"]
    online = Enum.map(agents, & &1["online"])

    assert Enum.any?(online)
    assert online == Enum.sort_by(online, &(not &1))
  end

  test "an agent shows the full name of an active owner and no other" do
    active = Seed.service(owner: :active, updated_at: Clock.now() + 100, online: true)
    claim = Seed.service(owner: :claim, updated_at: Clock.now() + 100, online: true)
    ended = Seed.service(owner: :expired, updated_at: Clock.now() + 100, online: true)
    alone = Seed.service(updated_at: Clock.now() + 100, online: true)

    owners = Map.new(Directory.snapshot()["agents"], &{&1["handle"], &1["owner"]})

    assert owners[active.handle] == active.owner
    assert owners[claim.handle] == nil
    assert owners[ended.handle] == nil
    assert owners[alone.handle] == nil
  end

  test "a listing that does not read is left out and the next agent takes its place" do
    before_view = Directory.snapshot()
    bad = Seed.service(bad: true, updated_at: Clock.now() + 100, online: true)
    # The other tests compare the counts with a view that decodes every row.
    on_exit(fn -> Listings.delete(bad.account.endpoint) end)
    view = Directory.snapshot()

    refute Enum.any?(view["agents"], &(&1["handle"] == bad.handle))
    assert length(view["agents"]) == @max_agents
    # The counts take no decoding: a bad row is a bug the page hides.
    assert view["listed"] == before_view["listed"] + 1
  end

  test "an expired listing is not shown or counted" do
    before_view = Directory.snapshot()
    ended = Seed.service(expires_at: Clock.now() - 1, updated_at: Clock.now() + 100, online: true)
    view = Directory.snapshot()

    refute Enum.any?(view["agents"], &(&1["handle"] == ended.handle))
    assert view["listed"] == before_view["listed"]
  end

  test "building the view takes a few queries, not one for each agent" do
    {_view, count} = queries(&Directory.snapshot/0)

    # The rows, the texts of the page, the owners, the people.
    assert count <= 5
  end

  describe "the route" do
    setup do
      previous = Application.fetch_env!(:aether_hub, :directory_cache_ms)
      Application.put_env(:aether_hub, :directory_cache_ms, 15_000)
      DirectoryCache.clear()

      on_exit(fn ->
        Application.put_env(:aether_hub, :directory_cache_ms, previous)
        DirectoryCache.clear()
      end)
    end

    defp directory, do: json(public_request(:get, "/directory.json"))

    test "serves the cached view, and a new one after the cache is cleared" do
      first = directory()
      late = Seed.service(updated_at: Clock.now() + 100, online: true)

      assert directory() == first
      refute Enum.any?(directory()["agents"], &(&1["handle"] == late.handle))

      DirectoryCache.clear()
      assert Enum.any?(directory()["agents"], &(&1["handle"] == late.handle))
    end

    test "reads the database once for many requests" do
      {_, count} =
        queries(fn -> for _ <- 1..10, do: public_request(:get, "/directory.json") end)

      # One build of a few queries, and no more for the other nine requests.
      assert count <= 5
    end

    test "keeps its shape and its cache header" do
      conn = public_request(:get, "/directory.json")

      assert conn.status == 200
      assert Plug.Conn.get_resp_header(conn, "cache-control") == ["public, max-age=15"]
      assert json(conn) |> Map.keys() |> Enum.sort() == AetherHub.Contract.keys("directory")

      for agent <- json(conn)["agents"] do
        assert agent |> Map.keys() |> Enum.sort() == AetherHub.Contract.keys("directoryAgent")
      end
    end
  end
end
