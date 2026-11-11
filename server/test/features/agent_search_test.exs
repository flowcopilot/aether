defmodule AetherHub.Features.AgentSearchTest do
  use AetherHub.Feature, file: "agent-search.feature", async: true

  alias AetherHub.Contract

  # The scenario's own word. Every listing and every search in a scenario has it.
  defp word(state), do: state[:word] || unique_name()

  defp searcher(_state, "a person"), do: claimed_node()
  defp searcher(state, name), do: actor(state, name)

  defp found(conn) do
    assert conn.status == 200
    json(conn)["agents"]
  end

  defp words(n), do: "q=" <> Enum.map_join(1..n, "+", &"word#{&1}")

  defp raw_query("a limit of 5 and no words"), do: "limit=5"
  defp raw_query(~s(the word "a")), do: "q=a"
  defp raw_query("only symbols"), do: "q=%22*%28"
  defp raw_query("9 words"), do: words(9)
  defp raw_query("8 words"), do: words(8)
  defp raw_query("a word of 129 letters"), do: "q=" <> String.duplicate("a", 129)

  defp raw_query(~s(the word "law" and a limit of ) <> limit), do: "q=law&limit=" <> limit

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

  defstep ~r/^(?<name>\w+) is a person with (?<contact>\w+) as a contact$/,
          %{name: name, contact: contact},
          state do
    friend = claimed_node()
    connect_by_request(friend, actor(state, contact))
    {:ok, put_actor(state, name, friend)}
  end

  # Searches

  defstep ~r/^(?<who>.+?) searches for "(?<words>.*)"$/, %{who: who, words: words}, state do
    query = "q=#{word(state)}+#{URI.encode_www_form(words)}"
    {:ok, %{found: found(search_agents(searcher(state, who), query))}}
  end

  defstep ~r/^(?<who>.+?) searches for all of them$/, %{who: who}, state do
    {:ok, %{found: found(search_agents(searcher(state, who), "q=#{word(state)}"))}}
  end

  defstep ~r/^a person sends the search query "(?<query>[^"]+)"$/, %{query: query}, state do
    query = String.replace(query, "{word}", word(state))
    {:ok, %{found: found(search_agents(claimed_node(), query))}}
  end

  defstep ~r/^a person searches with (?<search>.+)$/, %{search: search}, _state do
    {:ok, %{conn: search_agents(claimed_node(), raw_query(search))}}
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
    assert Enum.sort(Map.keys(row)) == Contract.keys("agent")
  end

  # Lookups

  defstep ~r/^(?<who>a person|\w+) looks up (?<name>\w+)$/, %{who: who, name: name}, state do
    {:ok,
     %{conn: request(searcher(state, who), "GET", name_path("/v1/agents", actor(state, name)))}}
  end

  defstep ~r/^a person looks up the name "(?<handle>[^"]+)"$/, %{handle: handle}, _state do
    path = name_path("/v1/agents", %{handle: handle})
    {:ok, %{conn: request(claimed_node(), "GET", path)}}
  end

  defstep ~r/^the answer is the listing of (?<name>\w+)$/, %{name: name}, %{conn: conn} = state do
    assert conn.status == 200
    assert json(conn)["handle"] == actor(state, name).handle
  end

  # The limit

  defstep ~r/^a person$/, _, _state do
    {:ok, %{asker: claimed_node()}}
  end

  defstep ~r/^the person searches 30 times$/, _, %{asker: asker} do
    start_of_window(:search)
    {:ok, %{statuses: for(_ <- 1..30, do: search_agents(asker, "q=law").status)}}
  end

  defstep ~r/^the person searches once more$/, _, %{asker: asker} do
    {:ok, %{conn: search_agents(asker, "q=law")}}
  end

  defstep ~r/^the person searches for people$/, _, %{asker: asker} do
    {:ok, %{conn: request(asker, "GET", "/v1/people?q=law")}}
  end
end
