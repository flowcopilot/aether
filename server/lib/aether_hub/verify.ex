defmodule AetherHub.Verify do
  @moduledoc """
  Guards every `/v1` request: refuses a node of an older protocol, reads the
  body (at most 16 KiB), checks the signed headers, and counts the request
  against the endpoint's rate limit.
  Only a request within the limit marks the endpoint as seen. On success it
  assigns `:endpoint` and `:body`.
  """
  @behaviour Plug

  import Plug.Conn

  @max_body 16 * 1024
  @max_skew 300
  @endpoint_format ~r/\A[0-9a-f]{64}\z/

  @impl true
  def init(opts), do: opts

  @impl true
  def call(%Plug.Conn{path_info: ["v1" | _]} = conn, _opts) do
    with :ok <- protocol(conn),
         {:ok, body, conn} <- read_limited(conn),
         {:ok, endpoint} <- authenticate(conn, body),
         :ok <- AetherHub.RateLimit.hit(:request, endpoint),
         :ok <- AetherHub.Presence.seen(endpoint) do
      conn |> assign(:endpoint, endpoint) |> assign(:body, body)
    else
      {:error, :upgrade_required} ->
        AetherHub.Router.error(conn, 426, "upgrade_required") |> halt()

      {:error, :too_large} ->
        AetherHub.Router.error(conn, 413, "too_large") |> halt()

      {:error, :unauthorized} ->
        AetherHub.Router.error(conn, 401, "unauthorized") |> halt()

      :rate_limited ->
        AetherHub.Router.error(conn, 429, "rate_limited") |> halt()
    end
  end

  def call(conn, _opts), do: conn

  @protocol "2"

  # A node names its hub protocol in `x-aether-protocol`. An older node
  # sends no header. Only `GET /v1/hub`, which tells a node the protocol of
  # this hub, is open to every node.
  defp protocol(%Plug.Conn{method: "GET", path_info: ["v1", "hub"]}), do: :ok

  defp protocol(conn) do
    case get_req_header(conn, "x-aether-protocol") do
      [@protocol] -> :ok
      _ -> {:error, :upgrade_required}
    end
  end

  defp read_limited(conn) do
    declared =
      case get_req_header(conn, "content-length") do
        [value | _] -> String.to_integer(value)
        [] -> 0
      end

    if declared > @max_body do
      {:error, :too_large}
    else
      case read_body(conn, length: @max_body, read_length: @max_body) do
        {:ok, body, conn} -> {:ok, body, conn}
        {:more, _partial, _conn} -> {:error, :too_large}
        {:error, _} -> {:error, :too_large}
      end
    end
  rescue
    ArgumentError -> {:error, :too_large}
  end

  @doc "The message a node signs for a request."
  def message(method, path_and_query, time, body) do
    Enum.join(
      [
        "aether-hub/v1",
        String.upcase(method),
        path_and_query,
        time,
        Base.encode16(:crypto.hash(:sha256, body), case: :lower)
      ],
      "\n"
    )
  end

  defp authenticate(conn, body) do
    with [endpoint] <- get_req_header(conn, "x-aether-endpoint"),
         [time] <- get_req_header(conn, "x-aether-time"),
         [signature] <- get_req_header(conn, "x-aether-signature"),
         true <- Regex.match?(@endpoint_format, endpoint),
         {seconds, ""} <- Integer.parse(time),
         true <- abs(System.system_time(:second) - seconds) <= @max_skew,
         {:ok, signature} <- Base.url_decode64(signature, padding: false),
         {:ok, key} <- Base.decode16(endpoint, case: :lower),
         message = message(conn.method, path_and_query(conn), time, body),
         true <- AetherHub.Signature.valid?(message, signature, key) do
      {:ok, endpoint}
    else
      _ -> {:error, :unauthorized}
    end
  end

  defp path_and_query(%Plug.Conn{request_path: path, query_string: ""}), do: path

  defp path_and_query(%Plug.Conn{request_path: path, query_string: query}),
    do: path <> "?" <> query
end
