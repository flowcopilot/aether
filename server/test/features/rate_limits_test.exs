defmodule AetherHub.Features.RateLimitsTest do
  use AetherHub.Feature, file: "rate-limits.feature", async: true

  import AetherHub.TestClient
  import AetherHub.HubStepsAccounts

  alias AetherHub.{Presence, RateLimit}

  defp account(node), do: request(node, "GET", "/v1/account")

  defp claim(node, ip, name \\ unique_name()) do
    request(node, "PUT", "/v1/account", %{"name" => name}, remote_ip: ip)
  end

  defp claims(count, ip), do: for(_ <- 1..count, do: claim(new_node(), ip).status)

  defstep ~r/^(?<who>\w+) is a new node$/, %{who: who}, state do
    {:ok, remember(state, who, new_node())}
  end

  # Requests a minute

  defstep ~r/^(?<who>\w+) (?:sends|sent) (?<count>\d+) requests in one minute$/,
          %{who: who, count: count},
          state do
    start_early_in_window(60, 10)
    node = who(state, who)
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
    assert json(account(who(state, who))) == %{"error" => error}
  end

  defstep ~r/^the hub still answers another node$/, _, _ do
    assert account(new_node()).status == 404
  end

  defstep ~r/^(?<who>\w+) was last seen long ago$/, %{who: who}, state do
    Presence.seen(who(state, who).endpoint, 1)
  end

  defstep ~r/^(?<who>\w+) sends one more request$/, %{who: who}, state do
    {:ok, %{conn: account(who(state, who))}}
  end

  defstep ~r/^the hub answers (?<status>\d+)$/, %{status: status}, %{conn: conn} do
    assert conn.status == String.to_integer(status)
  end

  defstep ~r/^(?<who>\w+) is still last seen long ago$/, %{who: who}, state do
    assert Presence.last_seen(who(state, who).endpoint) == 1
  end

  # New accounts an hour

  defstep ~r/^an address that made no accounts this hour$/, _, _ do
    start_early_in_window(3_600, 10)
    {:ok, %{ip: unique_ip()}}
  end

  defstep ~r/^(?<count>\d+) new nodes claim a name from that address$/,
          %{count: count},
          %{ip: ip} do
    {:ok, %{statuses: claims(String.to_integer(count), ip)}}
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

  # Buckets

  defstep ~r/^a key that made 30 searches this minute$/, _, _ do
    start_early_in_window(60, 5)
    key = make_ref()
    assert Enum.uniq(for _ <- 1..30, do: RateLimit.hit(:search, key)) == [:ok]
    {:ok, %{key: key}}
  end

  defstep ~r/^a 31st search of the key is refused$/, _, %{key: key} do
    assert RateLimit.hit(:search, key) == :rate_limited
  end

  defstep ~r/^a request of another kind from the key is allowed$/, _, %{key: key} do
    assert RateLimit.hit(:request, key) == :ok
  end
end
