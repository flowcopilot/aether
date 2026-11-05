defmodule AetherHub.Features.ClientAddressTest do
  use AetherHub.Feature, file: "client-address.feature", async: true

  alias AetherHub.ClientIp

  defp address(text) do
    {:ok, address} = text |> String.to_charlist() |> :inet.parse_address()
    address
  end

  defstep ~r/^the trusted proxies "(?<cidrs>[^"]*)"$/, %{cidrs: cidrs}, _state do
    {:ok, %{proxies: ClientIp.parse_cidrs!(cidrs)}}
  end

  defstep ~r/^"(?<peer>[^"]+)" sends a request forwarded for "(?<forwarded>[^"]*)"$/,
          %{peer: peer, forwarded: forwarded},
          %{proxies: proxies} do
    {:ok, %{client: ClientIp.client_ip(address(peer), [forwarded], proxies)}}
  end

  defstep ~r/^"(?<peer>[^"]+)" sends a request with these forwarded headers:$/,
          %{peer: peer, table: rows},
          %{proxies: proxies} do
    headers = Enum.map(rows, & &1["forwarded for"])
    {:ok, %{client: ClientIp.client_ip(address(peer), headers, proxies)}}
  end

  defstep ~r/^"(?<peer>[^"]+)" sends a request with no forwarded header$/,
          %{peer: peer},
          %{proxies: proxies} do
    {:ok, %{client: ClientIp.client_ip(address(peer), [], proxies)}}
  end

  defstep ~r/^the client address is "(?<address>[^"]+)"$/, %{address: expected}, %{client: client} do
    assert client == address(expected)
  end

  defstep ~r/^the operator sets the trusted proxies to "(?<range>[^"]*)"$/, %{range: range}, _ do
    {:ok, %{setting: fn -> ClientIp.parse_cidrs!(range) end}}
  end

  defstep ~r/^the hub trusts (?<count>\d+) ranges$/, %{count: count}, %{setting: setting} do
    assert length(setting.()) == String.to_integer(count)
  end

  defstep ~r/^the hub refuses the setting$/, _, %{setting: setting} do
    assert_raise ArgumentError, setting
  end
end
