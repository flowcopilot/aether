defmodule AetherHub.Features.LimitsTest do
  use AetherHub.Feature, file: "limits.feature", async: true

  import AetherHub.TestClient

  defstep ~r/^a node sends a body of 16 KiB and a few bytes more$/, _, _ do
    body = JSON.encode!(%{"name" => String.duplicate("a", 16 * 1024)})
    {:ok, %{conn: request(new_node(), "PUT", "/v1/account", body)}}
  end

  defstep ~r/^a node sends a body of exactly 16 KiB that is not a valid claim$/, _, _ do
    body = String.duplicate(" ", 16 * 1024 - 2) <> "{}"
    {:ok, %{conn: request(new_node(), "PUT", "/v1/account", body)}}
  end

  defstep ~r/^the hub answers (?<status>\d+)$/, %{status: status}, %{conn: conn} do
    assert conn.status == String.to_integer(status)
  end

  defstep ~r/^the hub answers (?<status>\d+) with the error "(?<error>\w+)"$/,
          %{status: status, error: error},
          %{conn: conn} do
    assert conn.status == String.to_integer(status)
    assert json(conn) == %{"error" => error}
  end
end
