defmodule AetherHub.Features.PeopleTest do
  use AetherHub.Feature, file: "people.feature", async: true

  defp lookup(asker, person), do: request(asker, "GET", name_path("/v1/people", person))

  alias AetherHub.Names

  defp search(asker, params), do: http_get(asker, "/v1/people", params)

  defp fill(text, %{word: word}), do: String.replace(text, "{word}", word)
  defp fill(text, _state), do: text

  # What a person finds: a contact also shows that it is online and its key.
  defp found(node, false), do: %{"handle" => node.handle, "contact" => false}

  defp found(node, true),
    do: %{
      "handle" => node.handle,
      "contact" => true,
      "online" => true,
      "endpoint" => node.endpoint
    }

  # Accounts

  defstep ~r/^a new search word$/, _, _ do
    {:ok, %{word: unique_name()}}
  end

  defstep ~r/^these accounts, whose names use the search word:$/, %{table: rows}, state do
    Enum.reduce(rows, state, fn row, state ->
      name = fill(row["name"], state)
      node = new_node()
      body = %{"name" => name}
      body = if row["type"] == "service", do: Map.put(body, "kind", "service"), else: body
      body = if row["searchable"] == "yes", do: Map.put(body, "searchable", true), else: body
      conn = request(node, "PUT", "/v1/account", body)
      assert conn.status == 200
      Map.merge(state, put_actor(state, row["name"], with_account(node, json(conn))))
    end)
    |> Map.take([:actors])
    |> then(&{:ok, &1})
  end

  # Lookups

  defstep ~r/^(?<who>\w+) looks up the tag of (?<other>\w+)$/, %{who: who, other: other}, state do
    {:ok, %{conn: lookup(actor(state, who), actor(state, other))}}
  end

  defstep ~r/^(?<who>\w+) looks up the name of (?<other>\w+) with another number$/,
          %{who: who, other: other},
          state do
    other = actor(state, other)
    other = %{other | handle: Names.format(other.name, other_number(other.number))}
    {:ok, %{conn: lookup(actor(state, who), other)}}
  end

  defstep ~r/^(?<who>\w+) looks up the name "(?<name>[^"]*)" with the number "(?<number>[^"]*)"$/,
          %{who: who, name: name, number: number},
          state do
    {:ok, %{conn: request(actor(state, who), "GET", "/v1/people/#{name}/#{number}")}}
  end

  defstep ~r/^(?<who>\w+) finds (?<other>\w+), not as a contact$/,
          %{other: other},
          %{conn: conn} = state do
    assert json(conn) == found(actor(state, other), false)
  end

  defstep ~r/^(?<who>\w+) finds (?<other>\w+) as a contact that is online, with his key$/,
          %{other: other},
          %{conn: conn} = state do
    assert json(conn) == found(actor(state, other), true)
  end

  # Searches

  defstep ~r/^(?<who>\w+) searches for "(?<word>[^"]*)"(?: with a limit of (?<limit>\S+))?$/,
          %{who: who, word: word, limit: limit},
          state do
    word = fill(word, state)
    params = if word == "", do: [], else: [q: word]
    params = if limit in ["", "none"], do: params, else: params ++ [limit: limit]
    {:ok, %{conn: search(actor(state, who), params)}}
  end

  defstep ~r/^(?<who>\w+) finds, in this order:$/, %{table: rows}, %{conn: conn} = state do
    expected = Enum.map(rows, &found(actor(state, &1["name"]), &1["contact"] == "yes"))
    assert json(conn)["people"] == expected
  end

  defstep ~r/^(?<who>\w+) finds nobody$/, _, %{conn: conn} do
    assert json(conn)["people"] == []
  end

  # Rate limit

  defstep ~r/^(?<who>\w+) looks up the tag of (?<other>\w+) (?<lookups>\d+) times and searches for his name (?<searches>\d+) times$/,
          %{who: who, other: other, lookups: lookups, searches: searches},
          state do
    asker = actor(state, who)
    other = actor(state, other)
    statuses = for _ <- 1..String.to_integer(lookups), do: lookup(asker, other).status

    statuses =
      statuses ++
        for _ <- 1..String.to_integer(searches), do: search(asker, q: other.name).status

    {:ok, %{statuses: statuses}}
  end

  defstep ~r/^the hub answers all (?<count>\d+)$/, %{count: count}, %{statuses: statuses} do
    assert length(statuses) == String.to_integer(count)
    assert Enum.uniq(statuses) == [200]
  end

  defstep ~r/^a search of (?<who>\w+) for the name of (?<other>\w+) gets 429$/,
          %{who: who, other: other},
          state do
    assert search(actor(state, who), q: actor(state, other).name).status == 429
  end

  defstep ~r/^another node can still look up the tag of (?<other>\w+)$/, %{other: other}, state do
    assert lookup(claimed_node(), actor(state, other)).status == 200
  end
end
