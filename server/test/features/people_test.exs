defmodule AetherHub.Features.PeopleTest do
  use AetherHub.Feature, file: "people.feature", async: true

  import AetherHub.TestClient
  import AetherHub.HubStepsAccounts

  defp lookup(asker, person), do: lookup(asker, person.name, person.number)

  defp lookup(asker, name, number) do
    digits = String.pad_leading(Integer.to_string(number), 4, "0")
    request(asker, "GET", "/v1/people/#{name}/#{digits}")
  end

  defp search(asker, params), do: request(asker, "GET", "/v1/people?" <> URI.encode_query(params))

  # Puts the scenario's search word in place of `{word}`.
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

  defstep ~r/^(?<who>\w+) has a name$/, %{who: who}, state do
    {:ok, remember(state, who, claimed_node())}
  end

  defstep ~r/^(?<who>\w+) has a service name$/, %{who: who}, state do
    {:ok, remember(state, who, claimed_node("service"))}
  end

  defstep ~r/^(?<who>\w+) is a new node$/, %{who: who}, state do
    {:ok, remember(state, who, new_node())}
  end

  defstep ~r/^(?<who>\w+) has read his invites$/, %{who: who}, state do
    {:ok, %{version: invites(who(state, who))["version"]}}
  end

  defstep ~r/^the invite version of (?<who>\w+) did not move$/,
          %{who: who},
          %{version: version} = state do
    assert invites(who(state, who))["version"] == version
  end

  defstep ~r/^(?<one>\S+) and (?<other>\S+) are contacts$/, %{one: one, other: other}, state do
    connect(who(state, one), who(state, other))
  end

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
      account = json(conn)
      node = Map.merge(node, %{handle: account["handle"], name: name, number: account["number"]})
      Map.merge(state, remember(state, row["name"], node))
    end)
    |> Map.take([:nodes])
    |> then(&{:ok, &1})
  end

  # Lookups

  defstep ~r/^(?<who>\w+) looks up the tag of (?<other>\w+)$/, %{who: who, other: other}, state do
    {:ok, %{conn: lookup(who(state, who), who(state, other))}}
  end

  defstep ~r/^(?<who>\w+) looks up the name of (?<other>\w+) with another number$/,
          %{who: who, other: other},
          state do
    other = who(state, other)
    {:ok, %{conn: lookup(who(state, who), other.name, other_number(other.number))}}
  end

  defstep ~r/^(?<who>\w+) looks up the name "(?<name>[^"]*)" with the number "(?<number>[^"]*)"$/,
          %{who: who, name: name, number: number},
          state do
    {:ok, %{conn: request(who(state, who), "GET", "/v1/people/#{name}/#{number}")}}
  end

  defstep ~r/^(?<who>\w+) finds (?<other>\w+), not as a contact$/,
          %{other: other},
          %{conn: conn} = state do
    assert json(conn) == found(who(state, other), false)
  end

  defstep ~r/^(?<who>\w+) finds (?<other>\w+) as a contact that is online, with his key$/,
          %{other: other},
          %{conn: conn} = state do
    assert json(conn) == found(who(state, other), true)
  end

  # Searches

  defstep ~r/^(?<who>\w+) searches for "(?<word>[^"]*)"(?: with a limit of (?<limit>\S+))?$/,
          %{who: who, word: word, limit: limit},
          state do
    word = fill(word, state)
    params = if word == "", do: [], else: [q: word]
    params = if limit in ["", "none"], do: params, else: params ++ [limit: limit]
    {:ok, %{conn: search(who(state, who), params)}}
  end

  defstep ~r/^(?<who>\w+) finds, in this order:$/, %{table: rows}, %{conn: conn} = state do
    expected = Enum.map(rows, &found(who(state, &1["name"]), &1["contact"] == "yes"))
    assert json(conn)["people"] == expected
  end

  defstep ~r/^(?<who>\w+) finds nobody$/, _, %{conn: conn} do
    assert json(conn)["people"] == []
  end

  # Rate limit

  defstep ~r/^(?<who>\w+) looks up the tag of (?<other>\w+) (?<lookups>\d+) times and searches for his name (?<searches>\d+) times$/,
          %{who: who, other: other, lookups: lookups, searches: searches},
          state do
    start_early_in_window(60, 10)
    asker = who(state, who)
    other = who(state, other)
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
    assert search(who(state, who), q: who(state, other).name).status == 429
  end

  defstep ~r/^another node can still look up the tag of (?<other>\w+)$/, %{other: other}, state do
    assert lookup(claimed_node(), who(state, other)).status == 200
  end

  # The answer

  defstep ~r/^the hub answers (?<status>\d+)$/, %{status: status}, %{conn: conn} do
    assert conn.status == String.to_integer(status)
  end

  defstep ~r/^the hub answers (?<status>\d+) with the error "(?<error>\w+)"$/,
          %{status: status, error: error},
          %{conn: conn} do
    assert conn.status == String.to_integer(status)
    assert json(conn) == %{"error" => error}
  end
end
