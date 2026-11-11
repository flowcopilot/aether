defmodule AetherHub.OwnerLinkLimitsTest do
  @moduledoc """
  The owner-link limits count only the links and claims that the nodes
  still see: an expired row that the sweeper has not removed yet counts as
  a swept one would, for nothing.
  """
  use ExUnit.Case, async: true

  import AetherHub.TestClient

  alias AetherHub.{Clock, Contract}

  @max_open_claims Contract.get(["limits", "ownerLinks", "maxOpenClaims"])
  @max_links Contract.get(["limits", "ownerLinks", "maxLinks"])

  setup do
    %{now: Clock.now()}
  end

  test "claims that expired before the sweep do not count as open claims", %{now: now} do
    personal = claimed_node()

    for _ <- 1..@max_open_claims do
      service = claimed_node("service")
      assert claim_owner(personal, service.handle, expires_at: now + 60).status == 201
    end

    Clock.set(now + 120)
    assert json(request(personal, "GET", "/v1/owner-links"))["links"] == []
    assert claim_owner(personal, claimed_node("service").handle).status == 201
  end

  test "links that expired before the sweep do not count toward the limit", %{now: now} do
    personal = claimed_node()

    for _ <- 1..@max_links do
      service = claimed_node("service")
      link_owner_until(personal, service, now + 60)
    end

    Clock.set(now + 120)
    assert json(request(personal, "GET", "/v1/owner-links"))["links"] == []
    assert claim_owner(personal, claimed_node("service").handle).status == 201
  end

  defp link_owner_until(personal, service, expires_at) do
    %{status: 201} = conn = claim_owner(personal, service.handle, expires_at: expires_at)
    link = Map.put(json(conn), "personalEndpoint", personal.endpoint)
    %{status: 200} = confirm_owner(service, link)
  end
end
