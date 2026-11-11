defmodule AetherHub.PresenceKeepTest do
  @moduledoc """
  A contact that the hub has not seen for the time that it keeps presence
  has no last-seen time, also before the sweeper removes the entry.
  """
  use ExUnit.Case, async: true

  import AetherHub.TestClient
  import AetherHub.HubSteps, only: [at: 2, connect: 2, contacts: 1]

  alias AetherHub.{Clock, Contract}

  @keep Contract.get(["limits", "presence", "keepSeconds"])

  test "the last-seen time is null after the keep time, before any sweep" do
    now = Clock.now()
    alice = claimed_node()
    bob = at(now - @keep - 1, fn -> claimed_node() end)
    at(now - @keep - 1, fn -> connect(bob, alice) end)

    assert [%{"handle" => handle, "lastSeen" => nil}] = contacts(alice)
    assert handle == bob.handle
  end

  test "the last-seen time stays up to the end of the keep time" do
    now = Clock.now()
    alice = claimed_node()
    bob = at(now - @keep, fn -> claimed_node() end)
    at(now - @keep, fn -> connect(bob, alice) end)

    assert [%{"lastSeen" => last_seen}] = at(now, fn -> contacts(alice) end)
    assert is_integer(last_seen)
  end
end
