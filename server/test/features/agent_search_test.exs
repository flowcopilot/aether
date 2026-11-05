defmodule AetherHub.Features.AgentSearchTest do
  use AetherHub.Feature, file: "agent-search.feature", async: true

  import AetherHub.TestClient
  import AetherHub.HubStepsSocial

  alias AetherHub.{Contact, Presence, Repo}

  # The scenario's own word. Every listing and every search in a scenario has it.
  defp word(state), do: state[:word] || marker()

  defp search(asker, query), do: request(asker, "GET", "/v1/agents?" <> query)

  defp searcher(_state, "a person"), do: claimed_node()
  defp searcher(state, name), do: actor(state, name)

  defp found(conn) do
    assert conn.status == 200
    json(conn)["agents"]
  end

  defp lookup_path(node),
    do: "/v1/agents/#{node.name}/#{String.pad_leading("#{node.number}", 4, "0")}"

  defp words(n), do: "q=" <> Enum.map_join(1..n, "+", &"word#{&1}")

  defp raw_query("a limit of 5 and no words"), do: "limit=5"
  defp raw_query(~s(the word "a")), do: "q=a"
  defp raw_query("only symbols"), do: "q=%22*%28"
  defp raw_query("9 words"), do: words(9)
  defp raw_query("8 words"), do: words(8)
  defp raw_query("a word of 129 letters"), do: "q=" <> String.duplicate("a", 129)

  defp raw_query(~s(the word "law" and a limit of ) <> limit), do: "q=law&limit=" <> limit

  defstep ~r/^(?<names>\w+(?:(?:, | and )\w+)*) (?:is an?|are) (?<kind>person|people|listed service agent|unlisted service agent|node without a name)$/,
          %{names: names, kind: kind},
          state do
    {:ok, new_actors(state, names, kind)}
  end

  defstep ~r/^the hub answers (?<status>\d+)$/, %{status: status}, state do
    assert_status(state, status)
  end

  defstep ~r/^the database of the hub has full-text search$/, _, _state do
    assert %{rows: [[1]]} = Repo.query!("SELECT sqlite_compileoption_used('ENABLE_FTS5')")
  end

  # Listings

  defstep ~r/^(?<name>\w+) is listed with "(?<text>[^"]*)"(?: (?<times>\d+) times)?(?:, the skill "(?<skill>[^"]+)")?(?:,? and the tag "(?<tag>[^"]+)")?$/,
          %{name: name, text: text, times: times, skill: skill, tag: tag},
          state do
    word = word(state)
    times = if times == "", do: 1, else: String.to_integer(times)
    opts = [description: "#{word} " <> String.duplicate(text, times)]
    opts = if skill == "", do: opts, else: [{:skill, skill} | opts]
    opts = if tag == "", do: opts, else: [{:tags, [tag]} | opts]
    {:ok, Map.put(put_actor(state, name, listed_service(opts)), :word, word)}
  end

  defstep ~r/^the listing of (?<name>\w+) is a minute older$/, %{name: name}, state do
    Repo.query!("UPDATE listings SET updated_at = updated_at - 60 WHERE endpoint = ?1", [
      actor(state, name).endpoint
    ])
  end

  defstep ~r/^(?<name>\w+) was last online long ago$/, %{name: name}, state do
    Presence.seen(actor(state, name).endpoint, 1)
  end

  defstep ~r/^(?<name>\w+) is a person with (?<contact>\w+) as a contact$/,
          %{name: name, contact: contact},
          state do
    friend = claimed_node()

    Repo.insert_all(Contact, [
      %{owner_endpoint: friend.endpoint, contact_endpoint: actor(state, contact).endpoint}
    ])

    {:ok, put_actor(state, name, friend)}
  end

  # Searches

  defstep ~r/^(?<who>.+?) searches for "(?<words>.*)"$/, %{who: who, words: words}, state do
    query = "q=#{word(state)}+#{URI.encode_www_form(words)}"
    {:ok, %{found: found(search(searcher(state, who), query))}}
  end

  defstep ~r/^(?<who>.+?) searches for all of them$/, %{who: who}, state do
    {:ok, %{found: found(search(searcher(state, who), "q=#{word(state)}"))}}
  end

  defstep ~r/^a person sends the search query "(?<query>[^"]+)"$/, %{query: query}, state do
    query = String.replace(query, "{word}", word(state))
    {:ok, %{found: found(search(claimed_node(), query))}}
  end

  defstep ~r/^a person searches with (?<search>.+)$/, %{search: search}, _state do
    {:ok, %{conn: search(claimed_node(), raw_query(search))}}
  end

  defstep ~r/^the search finds (?<result>nothing|\d+ agents|\w+(?:, \w+)*)$/,
          %{result: result},
          %{found: found} = state do
    handles = Enum.map(found, & &1["handle"])

    case Regex.run(~r/^(\d+) agents$/, result) do
      [_, n] ->
        assert length(handles) == String.to_integer(n)

      nil when result == "nothing" ->
        assert handles == []

      nil ->
        expected = for name <- String.split(result, ", "), do: actor(state, name).handle
        assert handles == expected
    end
  end

  defstep ~r/^the search finds (?<online>\w+) online, then (?<offline>\w+) offline$/,
          %{online: online, offline: offline},
          %{found: found} = state do
    assert [
             %{"handle" => online_handle, "online" => true},
             %{"handle" => offline_handle, "online" => false}
           ] = found

    assert {online_handle, offline_handle} ==
             {actor(state, online).handle, actor(state, offline).handle}
  end

  defstep ~r/^the search shows (?<name>\w+) as a contact, with its endpoint$/,
          %{name: name},
          %{found: found} = state do
    assert [%{"contact" => true, "endpoint" => endpoint}] = found
    assert endpoint == actor(state, name).endpoint
  end

  defstep ~r/^the search shows \w+ as no contact, without its endpoint$/, _, %{found: found} do
    assert [row] = found
    assert row["contact"] == false
    refute Map.has_key?(row, "endpoint")
  end

  defstep ~r/^the row shows only the listing, its signature and expiry, the full name, the owner, and whether it is online and a contact$/,
          _,
          %{found: [row]} do
    assert Enum.sort(Map.keys(row)) ==
             ~w(contact expiresAt handle listing online owner signature)
  end

  # Lookups

  defstep ~r/^(?<who>a person|\w+) looks up (?<name>\w+)$/, %{who: who, name: name}, state do
    {:ok, %{conn: request(searcher(state, who), "GET", lookup_path(actor(state, name)))}}
  end

  defstep ~r/^a person looks up the name "(?<handle>[^"]+)"$/, %{handle: handle}, _state do
    [name, number] = String.split(handle, "#")
    {:ok, %{conn: request(claimed_node(), "GET", "/v1/agents/#{name}/#{number}")}}
  end

  defstep ~r/^the answer is the listing of (?<name>\w+)$/, %{name: name}, %{conn: conn} = state do
    assert conn.status == 200
    assert json(conn)["handle"] == actor(state, name).handle
  end

  # The limit

  defstep ~r/^a person at the start of a minute$/, _, _state do
    start_early_in_window(60, 10)
    {:ok, %{asker: claimed_node()}}
  end

  defstep ~r/^the person searches 30 times$/, _, %{asker: asker} do
    {:ok, %{statuses: for(_ <- 1..30, do: search(asker, "q=law").status)}}
  end

  defstep ~r/^the hub answers 200 each time$/, _, state do
    assert Enum.uniq(state.statuses) == [200]
  end

  defstep ~r/^the person searches once more$/, _, %{asker: asker} do
    {:ok, %{conn: search(asker, "q=law")}}
  end

  defstep ~r/^the person searches for people$/, _, %{asker: asker} do
    {:ok, %{conn: request(asker, "GET", "/v1/people?q=law")}}
  end
end
