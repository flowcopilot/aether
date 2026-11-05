defmodule AetherHub.Verify do
  @moduledoc """
  Guards every `/v1` request: refuses a node of an older protocol, reads the
  body, and checks the signed headers. On success it assigns `:endpoint` and
  `:body`.
  """
  @behaviour Plug

  import Plug.Conn

  alias AetherHub.{Clock, Protocol, Reply, Signed}

  @max_body Protocol.limit(:max_body_bytes)
  @max_skew Protocol.limit(:max_skew_seconds)
  @protocol Integer.to_string(Protocol.version())

  @impl true
  def init(opts), do: opts

  @impl true
  def call(%Plug.Conn{path_info: ["v1" | _]} = conn, _opts) do
    with :ok <- protocol(conn),
         {:ok, body, conn} <- read_limited(conn),
         {:ok, endpoint} <- authenticate(conn, body) do
      conn |> assign(:endpoint, endpoint) |> assign(:body, body)
    else
      {:error, code} -> conn |> Reply.error(code) |> halt()
    end
  end

  def call(conn, _opts), do: conn

  # A node names its hub protocol in a header; an older node sends none.
  # Only `GET /v1/hub`, which tells a node the protocol of this hub, is open
  # to every node.
  defp protocol(%Plug.Conn{method: "GET", path_info: ["v1", "hub"]}), do: :ok

  defp protocol(conn) do
    case get_req_header(conn, Protocol.header(:protocol)) do
      [@protocol] -> :ok
      _ -> {:error, :upgrade_required}
    end
  end

  defp read_limited(conn) do
    case get_req_header(conn, "content-length") do
      [] -> read_up_to_limit(conn)
      [value | _] -> read_declared(conn, Integer.parse(value))
    end
  end

  defp read_declared(conn, {declared, ""}) when declared in 0..@max_body,
    do: read_up_to_limit(conn)

  defp read_declared(_conn, {declared, ""}) when declared > @max_body, do: {:error, :too_large}
  defp read_declared(_conn, _parsed), do: {:error, :invalid}

  defp read_up_to_limit(conn) do
    case read_body(conn, length: @max_body, read_length: @max_body) do
      {:ok, body, conn} -> {:ok, body, conn}
      {:more, _partial, _conn} -> {:error, :too_large}
      {:error, _} -> {:error, :too_large}
    end
  end

  defp authenticate(conn, body) do
    with [endpoint] <- get_req_header(conn, Protocol.header(:endpoint)),
         [time] <- get_req_header(conn, Protocol.header(:time)),
         [signature] <- get_req_header(conn, Protocol.header(:signature)),
         true <- Regex.match?(Protocol.regex(:endpoint), endpoint),
         {seconds, ""} <- Integer.parse(time),
         true <- abs(Clock.now() - seconds) <= @max_skew,
         message = Signed.request_message(conn.method, path_and_query(conn), time, body),
         true <- Signed.valid?(endpoint, message, signature) do
      {:ok, endpoint}
    else
      _ -> {:error, :unauthorized}
    end
  end

  defp path_and_query(%Plug.Conn{request_path: path, query_string: ""}), do: path

  defp path_and_query(%Plug.Conn{request_path: path, query_string: query}),
    do: path <> "?" <> query
end
