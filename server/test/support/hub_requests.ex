defmodule AetherHub.HubRequests do
  @moduledoc """
  Shared GET helpers for hub feature tests.
  """

  import ExUnit.Assertions
  import AetherHub.TestClient

  @doc "GET `path` on `node`, with an optional query map or query string."
  def get(node, path, query \\ nil) do
    path =
      case query do
        nil -> path
        "" -> path
        q when is_map(q) or is_list(q) -> path <> "?" <> URI.encode_query(q)
        q when is_binary(q) -> path <> "?" <> q
      end

    request(node, "GET", path)
  end

  @doc "The `field` of a successful GET response, or the field when `status` is nil."
  def field(node, path, field, query \\ nil, status \\ 200) do
    conn = get(node, path, query)
    if status, do: assert(conn.status == status)
    json(conn)[field]
  end
end
