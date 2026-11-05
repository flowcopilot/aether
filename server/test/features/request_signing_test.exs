defmodule AetherHub.Features.RequestSigningTest do
  use AetherHub.Feature, file: "request-signing.feature", async: true

  import Plug.Test
  import AetherHub.TestClient
  import AetherHub.HubStepsAccounts

  @opts AetherHub.Router.init([])

  defp now, do: System.system_time(:second)

  defstep ~r/^(?<who>\w+) has a name$/, %{who: who}, state do
    {:ok, remember(state, who, claimed_node())}
  end

  defstep ~r/^a monitor asks the hub for its health$/, _, _ do
    {:ok, %{conn: AetherHub.Router.call(conn(:get, "/healthz"), @opts)}}
  end

  defstep ~r/^a node sends a request without a signature$/, _, _ do
    conn =
      :get
      |> conn("/v1/account")
      |> Plug.Conn.put_req_header("x-aether-protocol", "2")
      |> AetherHub.Router.call(@opts)

    {:ok, %{conn: conn}}
  end

  defstep ~r/^a node without an account sends a correctly signed request$/, _, _ do
    {:ok, %{conn: request(new_node(), "GET", "/v1/account")}}
  end

  defstep ~r/^(?<who>\w+) reads her invites with a query in a correctly signed request$/,
          %{who: who},
          state do
    {:ok, %{conn: request(who(state, who), "GET", "/v1/invites?after=0&wait=0")}}
  end

  defstep ~r/^(?<who>\w+) sends a request (?<problem>(signed|with) .+)$/,
          %{who: who, problem: problem},
          state do
    {:ok, %{conn: badly_signed(who(state, who), problem)}}
  end

  defp badly_signed(node, "signed by another key"),
    do: request(%{node | private: new_node().private}, "GET", "/v1/account")

  defp badly_signed(node, "with a malformed signature"),
    do: request(node, "GET", "/v1/account", nil, signature: "not-a-signature")

  defp badly_signed(node, "signed 301 seconds ago"),
    do: request(node, "GET", "/v1/account", nil, time: Integer.to_string(now() - 301))

  defp badly_signed(node, "signed 301 seconds in the future"),
    do: request(node, "GET", "/v1/account", nil, time: Integer.to_string(now() + 301))

  defp badly_signed(node, "signed over another body"),
    do: request(node, "PUT", "/v1/account", %{"name" => unique_name()}, signed_body: "{}")

  defp badly_signed(node, "signed for another query") do
    time = Integer.to_string(now())
    message = AetherHub.Verify.message("GET", "/v1/invites?after=0", time, "")
    signature = :crypto.sign(:eddsa, :none, message, [node.private, :ed25519])

    request(node, "GET", "/v1/invites?after=1", nil,
      time: time,
      signature: Base.url_encode64(signature, padding: false)
    )
  end

  defstep ~r/^a node sends a request (?<protocol>without a protocol version|for protocol \d+)$/,
          %{protocol: protocol},
          _ do
    {:ok, %{conn: request(new_node(), "GET", "/v1/account", nil, protocol: protocol(protocol))}}
  end

  defstep ~r/^a node without an account asks the hub for its protocol (?<protocol>.+)$/,
          %{protocol: protocol},
          _ do
    {:ok, %{conn: request(new_node(), "GET", "/v1/hub", nil, protocol: protocol(protocol))}}
  end

  defp protocol("without a protocol version"), do: nil
  defp protocol("for protocol " <> version), do: version
  defp protocol("as protocol " <> version), do: version

  defstep ~r/^the hub answers (?<status>\d+)$/, %{status: status}, %{conn: conn} do
    assert conn.status == String.to_integer(status)
  end

  defstep ~r/^the hub answers (?<status>\d+) with "(?<body>[^"]*)"$/,
          %{status: status, body: body},
          %{conn: conn} do
    assert conn.status == String.to_integer(status)
    assert conn.resp_body == body
  end

  defstep ~r/^the hub answers (?<status>\d+) with the error "(?<error>\w+)"$/,
          %{status: status, error: error},
          %{conn: conn} do
    assert conn.status == String.to_integer(status)
    assert json(conn) == %{"error" => error}
  end

  defstep ~r/^the hub answers that it speaks protocol (?<version>\d+)$/,
          %{version: version},
          %{conn: conn} do
    assert conn.status == 200
    assert json(conn) == %{"protocol" => String.to_integer(version)}
  end
end
