defmodule AetherHub.Features.RequestSigningTest do
  use AetherHub.Feature, file: "request-signing.feature", async: true

  import Plug.Test

  alias AetherHub.Contract

  @opts AetherHub.Router.init([])

  defp now, do: System.system_time(:second)

  defstep ~r/^a monitor asks the hub for its health$/, _, _ do
    {:ok, %{conn: AetherHub.Router.call(conn(:get, "/healthz"), @opts)}}
  end

  defstep ~r/^a node sends a request without a signature$/, _, _ do
    conn =
      :get
      |> conn("/v1/account")
      |> Plug.Conn.put_req_header(
        Contract.get(["headers", "protocol"]),
        Integer.to_string(Contract.get(["protocol"]))
      )
      |> AetherHub.Router.call(@opts)

    {:ok, %{conn: conn}}
  end

  defstep ~r/^a node without an account sends a correctly signed request$/, _, _ do
    {:ok, %{conn: request(new_node(), "GET", "/v1/account")}}
  end

  defstep ~r/^(?<who>\w+) reads her invites with a query in a correctly signed request$/,
          %{who: who},
          state do
    {:ok, %{conn: request(actor(state, who), "GET", "/v1/invites?after=0&wait=0")}}
  end

  defstep ~r/^(?<who>\w+) sends a request (?<problem>(signed|with) .+)$/,
          %{who: who, problem: problem},
          state do
    {:ok, %{conn: badly_signed(actor(state, who), problem)}}
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

    message =
      Contract.message("request", %{
        "method" => "GET",
        "pathAndQuery" => "/v1/invites?after=0",
        "time" => time,
        "body" => ""
      })

    request(node, "GET", "/v1/invites?after=1", nil, time: time, signature: sign(node, message))
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

  defstep ~r/^the hub answers (?<status>\d+) with "(?<body>[^"]*)"$/,
          %{status: status, body: body},
          %{conn: conn} do
    assert conn.status == String.to_integer(status)
    assert conn.resp_body == body
  end

  defstep ~r/^the hub answers that it speaks protocol (?<version>\d+)$/,
          %{version: version},
          %{conn: conn} do
    assert conn.status == 200
    assert json(conn) == %{"protocol" => String.to_integer(version)}
  end
end
