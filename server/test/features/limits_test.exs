defmodule AetherHub.Features.LimitsTest do
  use AetherHub.Feature, file: "limits.feature", async: true

  @max_body AetherHub.Contract.get(["limits", "maxBodyBytes"])

  defstep ~r/^a node sends a body of 16 KiB and a few bytes more$/, _, _ do
    body = JSON.encode!(%{"name" => String.duplicate("a", @max_body)})
    {:ok, %{conn: request(new_node(), "PUT", "/v1/account", body)}}
  end

  defstep ~r/^a node sends a body of exactly 16 KiB that is not a valid claim$/, _, _ do
    body = String.duplicate(" ", @max_body - 2) <> "{}"
    {:ok, %{conn: request(new_node(), "PUT", "/v1/account", body)}}
  end
end
