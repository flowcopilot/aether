defmodule AetherHub.AgentSearchBatchTest do
  @moduledoc """
  The search reads the owners of all its results in one query. Its results,
  and their order, are the ones that the search had with one query for the
  owner of each result.
  """
  use ExUnit.Case, async: true

  import AetherHub.TestClient

  alias AetherHub.{Account, Clock, Contacts, Listings, Names, OwnerLinks, Presence, Repo, Seed}

  # The search as it was: the same query, and `active_owner/1` for each row.
  defp reference(%Account{endpoint: asker}, words, limit) do
    match = Enum.map_join(words, " ", &(~s(") <> &1 <> ~s("*)))

    %{rows: rows} =
      Repo.query!(
        """
        SELECT l.endpoint, l.listing, l.signature, l.expires_at, a.name, a.number
        FROM listing_search
        JOIN listings l ON l.id = listing_search.rowid
        JOIN accounts a ON a.endpoint = l.endpoint
        WHERE listing_search MATCH ?1 AND l.expires_at > ?2
        ORDER BY bm25(listing_search), l.updated_at DESC
        LIMIT ?3
        """,
        [match, Clock.now(), limit]
      )

    contacts = Contacts.known(asker, Enum.map(rows, &hd/1))

    agents =
      rows
      |> Enum.map(fn [endpoint, text, signature, expires_at, name, number] ->
        contact = MapSet.member?(contacts, endpoint)

        row = %{
          "handle" => Names.format(name, number),
          "listing" => text,
          "signature" => signature,
          "expiresAt" => expires_at,
          "online" => Presence.online?(endpoint),
          "contact" => contact,
          "owner" => OwnerLinks.active_owner(endpoint)
        }

        if contact, do: Map.put(row, "endpoint", endpoint), else: row
      end)
      |> Enum.sort_by(&(not &1["online"]))

    {:ok, %{"agents" => agents}}
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

  # Listings that all hold `word`, some more often than others so that the
  # ranks differ, and some with the same rank and the same update time.
  setup do
    word = unique_name()
    now = Clock.now()
    asker = Seed.personal()

    agents =
      for i <- 1..30 do
        repeats = String.duplicate(word <> " ", rem(i, 4) + 1)

        Seed.service(
          description: "#{repeats}agent #{i}",
          updated_at: now - rem(i, 3),
          online: rem(i, 5) == 0,
          owner: Enum.at([nil, :active, :claim, :expired], rem(i, 4))
        )
      end

    for agent <- Enum.take_every(agents, 6), do: Seed.contact(asker, agent.account)

    %{word: word, asker: asker, agents: agents}
  end

  test "the results and their order are the ones of the search before", %{
    word: word,
    asker: asker
  } do
    for limit <- [1, 3, 7, 10, 20] do
      assert Listings.search(asker, [word], limit) == reference(asker, [word], limit)
    end

    assert {:ok, %{"agents" => agents}} = Listings.search(asker, [word], 20)
    assert length(agents) == 20
  end

  test "more than one word and a prefix give the same results", %{word: word, asker: asker} do
    prefix = String.slice(word, 0, 5)

    for words <- [[word, "agent"], [prefix], ["agent", word, "answers"]], limit <- [5, 20] do
      assert Listings.search(asker, words, limit) == reference(asker, words, limit)
    end
  end

  test "the online agents come first inside the page", %{word: word, asker: asker} do
    {:ok, %{"agents" => agents}} = Listings.search(asker, [word], 20)
    online = Enum.map(agents, & &1["online"])

    assert Enum.any?(online) and not Enum.all?(online)
    assert online == Enum.sort_by(online, &(not &1))
  end

  test "each result shows its own active owner and no other", %{
    word: word,
    asker: asker,
    agents: agents
  } do
    {:ok, %{"agents" => found}} = Listings.search(asker, [word], 20)
    expected = Map.new(agents, &{&1.handle, &1.owner})

    # One listing in four has an active link, and the others have a claim,
    # an ended link or none: only the active link shows an owner.
    assert Enum.count(found, & &1["owner"]) > 0
    assert Enum.count(found, &(&1["owner"] == nil)) > 0
    assert Enum.all?(found, &(&1["owner"] == expected[&1["handle"]]))
  end

  test "an expired listing is not found", %{word: word, asker: asker} do
    ended = Seed.service(description: word, expires_at: Clock.now() - 1)
    {:ok, %{"agents" => agents}} = Listings.search(asker, [word], 20)

    refute Enum.any?(agents, &(&1["handle"] == ended.handle))
  end

  test "a search takes three queries however many results it has", %{word: word, asker: asker} do
    {{:ok, %{"agents" => agents}}, count} = queries(fn -> Listings.search(asker, [word], 20) end)

    assert length(agents) == 20
    # The matches, the contacts, the owners.
    assert count == 3
  end

  test "a search with no result takes the same queries", %{asker: asker} do
    {{:ok, %{"agents" => agents}}, count} =
      queries(fn -> Listings.search(asker, [unique_name()], 20) end)

    assert agents == []
    assert count <= 3
  end

  test "the route answers with the same results", %{word: word} do
    node = claimed_node()
    conn = request(node, "GET", "/v1/agents?q=#{word}&limit=20")
    assert conn.status == 200

    account = Repo.get!(Account, node.endpoint)
    assert json(conn) == JSON.decode!(JSON.encode!(elem(reference(account, [word], 20), 1)))
  end
end
