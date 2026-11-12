defmodule AetherHub.Features.RateLimitsTest do
  use AetherHub.Feature, file: "rate-limits.feature", async: true

  alias AetherHub.Presence

  defp account(node), do: request(node, "GET", "/v1/account")

  defp claim(node, ip, name \\ unique_name()) do
    request(node, "PUT", "/v1/account", %{"name" => name}, remote_ip: ip)
  end

  defp claims(count, ip), do: for(_ <- 1..count, do: claim(new_node(), ip).status)

  # Requests a minute

  defstep ~r/^(?<who>\w+) (?:sends|sent) (?<count>\d+) requests in one minute$/,
          %{who: who, count: count},
          state do
    node = actor(state, who)
    start_of_window(:request)

    {:ok, %{statuses: for(_ <- 1..String.to_integer(count), do: account(node).status)}}
  end

  defstep ~r/^the hub answers the first 120 and refuses the last with 429$/,
          _,
          %{statuses: statuses} do
    assert Enum.take(statuses, 120) |> Enum.uniq() == [404]
    assert List.last(statuses) == 429
  end

  defstep ~r/^the next request of (?<who>\w+) gets the error "(?<error>\w+)"$/,
          %{who: who, error: error},
          state do
    assert json(account(actor(state, who))) == %{"error" => error}
  end

  defstep ~r/^the hub still answers another node$/, _, _ do
    assert account(new_node()).status == 404
  end

  defstep ~r/^(?<who>\w+) sends one more request$/, %{who: who}, state do
    {:ok, %{conn: account(actor(state, who))}}
  end

  defstep ~r/^(?<who>\w+) is still last seen long ago$/, %{who: who}, state do
    assert Presence.last_seen(actor(state, who).endpoint) == state.seen_at
  end

  # New accounts an hour

  defstep ~r/^an address that made no accounts this hour$/, _, _ do
    {:ok, %{ip: unique_ip()}}
  end

  defstep ~r/^(?<count>\d+) new nodes claim a name from that address$/,
          %{count: count},
          %{ip: ip} do
    {:ok, %{statuses: claims(String.to_integer(count), ip)}}
  end

  defstep ~r/^(?<count>\d+) new nodes from that address claim a name that is not valid$/,
          %{count: count},
          %{ip: ip} do
    for _ <- 1..String.to_integer(count),
        do: assert(claim(new_node(), ip, "Not Valid").status == 400)
  end

  defstep ~r/^one new node claims the same name (?<count>\d+) times from that address$/,
          %{count: count},
          %{ip: ip} do
    node = new_node()
    name = unique_name()
    {:ok, %{statuses: for(_ <- 1..String.to_integer(count), do: claim(node, ip, name).status)}}
  end

  defstep ~r/^the hub accepts all (?<count>\d+) claims$/, %{count: count}, %{statuses: statuses} do
    assert length(statuses) == String.to_integer(count)
    assert Enum.uniq(statuses) == [200]
  end

  defstep ~r/^a 21st new node from that address gets 429 with the error "(?<error>\w+)"$/,
          %{error: error},
          %{ip: ip} do
    conn = claim(new_node(), ip)
    assert conn.status == 429
    assert json(conn) == %{"error" => error}
  end

  defstep ~r/^a new node from that address gets 429$/, _, %{ip: ip} do
    assert claim(new_node(), ip).status == 429
  end

  defstep ~r/^a new node from another address can claim a name$/, _, _ do
    assert claim(new_node(), unique_ip()).status == 200
  end

  # Kinds of requests

  defstep ~r/^(?<who>\w+) made 30 searches$/, %{who: who}, state do
    node = actor(state, who)
    assert Enum.uniq(for _ <- 1..30, do: search_people(node, "q=ab").status) == [200]
  end

  defstep ~r/^(?<who>\w+) searches once more$/, %{who: who}, state do
    {:ok, %{conn: search_people(actor(state, who), "q=ab")}}
  end

  defstep ~r/^(?<who>\w+) reads her account$/, %{who: who}, state do
    {:ok, %{conn: account(actor(state, who))}}
  end
end
