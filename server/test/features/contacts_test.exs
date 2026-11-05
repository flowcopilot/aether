defmodule AetherHub.Features.ContactsTest do
  use AetherHub.Feature, file: "contacts.feature", async: true

  alias AetherHub.{Clock, Contract, Presence, Sweeper}

  @step Contract.get(["limits", "presence", "lastSeenStepSeconds"])
  @keep Contract.get(["limits", "presence", "keepSeconds"])

  defp now, do: System.system_time(:second)
  defp rounded(time), do: time - rem(time, @step)

  defstep ~r/^(?<first>\w+) and (?<second>\w+) have names, and the handle of \k<first> sorts first$/,
          %{first: first, second: second},
          state do
    [one, two] = Enum.sort_by([claimed_node(), claimed_node()], & &1.handle)
    {:ok, state |> put_actor(first, one) |> put_actor(second, two)}
  end

  defstep ~r/^(?<who>\w+) changes his name$/, %{who: who}, state do
    conn = request(actor(state, who), "PUT", "/v1/account", %{"name" => unique_name()})
    assert conn.status == 200
    {:ok, %{new_handle: json(conn)["handle"]}}
  end

  # Invites and contacts

  defstep ~r/^(?<one>\w+) and (?<other>\w+) become contacts again with a new invite$/,
          %{one: one, other: other},
          state do
    connect(actor(state, one), actor(state, other))
  end

  defstep ~r/^(?<who>\w+) invites (?<other>\w+)$/, %{who: who, other: other}, state do
    {:ok, %{invite_id: json(invite(actor(state, who), actor(state, other)))["id"]}}
  end

  defstep ~r/^(?<who>\w+) accepts the invite$/, %{who: who}, %{invite_id: id} = state do
    assert request(actor(state, who), "POST", "/v1/invites/#{id}/accept").status == 200
  end

  defstep ~r/^(?<who>\w+) deletes the invite$/, %{who: who}, %{invite_id: id} = state do
    {:ok, %{conn: request(actor(state, who), "DELETE", "/v1/invites/#{id}")}}
  end

  defstep ~r/^(?<who>\w+) has only (?<other>\w+) as a contact$/,
          %{who: who, other: other},
          state do
    other = actor(state, other)
    assert [%{"handle" => handle, "endpoint" => endpoint}] = contacts(actor(state, who))
    assert {handle, endpoint} == {other.handle, other.endpoint}
  end

  defstep ~r/^(?<who>\w+) has (?<count>\d+) contacts?$/, %{who: who, count: count}, state do
    assert length(contacts(actor(state, who))) == String.to_integer(count)
  end

  defstep ~r/^(?<who>\w+) sees her contacts in this order: (?<names>.+)$/,
          %{who: who, names: names},
          state do
    expected = names |> String.split(", ") |> Enum.map(&actor(state, &1).handle)
    assert Enum.map(contacts(actor(state, who)), & &1["handle"]) == expected
  end

  defstep ~r/^(?<who>\w+) sees the new handle of (?<other>\w+), as a personal agent$/,
          %{who: who},
          %{new_handle: new_handle} = state do
    assert [%{"handle" => ^new_handle, "kind" => "personal"}] = contacts(actor(state, who))
  end

  defstep ~r/^(?<who>\w+) removes (?<other>\w+) from (?:her|his) contacts$/,
          %{who: who, other: other},
          state do
    path = "/v1/contacts/#{actor(state, other).endpoint}"
    {:ok, %{conn: request(actor(state, who), "DELETE", path)}}
  end

  defstep ~r/^(?<who>\w+) reads his contacts$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "GET", "/v1/contacts")}}
  end

  # Presence

  defstep ~r/^(?<who>\w+) was last seen (?<seconds>\d+) seconds ago$/,
          %{who: who, seconds: seconds},
          state do
    seen_at = now() - String.to_integer(seconds)
    seen_at(actor(state, who), seen_at)
    {:ok, %{seen_at: seen_at}}
  end

  defstep ~r/^(?<who>\w+) was last seen one hour ago, in the last second of a 15-minute block$/,
          %{who: who},
          state do
    seen = rounded(now() - 3_600) + @step - 1
    seen_at(actor(state, who), seen)
    {:ok, %{seen_at: seen}}
  end

  defstep ~r/^(?<who>\w+) sends a signed request$/, %{who: who}, state do
    sent_at = now()
    request(actor(state, who), "GET", "/v1/account")
    {:ok, %{sent_at: sent_at}}
  end

  defstep ~r/^(?<who>\w+) waits for news for (?<seconds>\d+) seconds$/,
          %{who: who, seconds: seconds},
          %{seen_at: seen_at} = state do
    node = actor(state, who)
    version = at(seen_at, fn -> invites(node)["version"] end)

    # The poll is sent at the last-seen time, so only the open poll can show
    # the node online.
    task =
      Task.async(fn ->
        Clock.set(seen_at)
        request(node, "GET", "/v1/invites?after=#{version}&wait=#{seconds}")
      end)

    # The poll needs a moment to reach its wait.
    Process.sleep(200)
    {:ok, %{wait: task}}
  end

  defstep ~r/^the wait of (?<who>\w+) ends$/, _, %{wait: task} do
    assert Task.await(task, 5_000).status == 200
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) (?<presence>online|offline)$/,
          %{who: who, presence: presence},
          state do
    online = presence == "online"
    assert [%{"online" => ^online}] = contacts(actor(state, who))
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) (?<presence>online|offline), last seen (?<seconds>\d+) seconds ago rounded down to 15 minutes$/,
          %{who: who, presence: presence},
          %{seen_at: seen_at} = state do
    online = presence == "online"
    assert [%{"online" => ^online, "lastSeen" => last_seen}] = contacts(actor(state, who))
    assert last_seen == rounded(seen_at)
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) online, last seen now rounded down to 15 minutes$/,
          %{who: who},
          %{sent_at: sent_at} = state do
    assert [%{"online" => true, "lastSeen" => last_seen}] = contacts(actor(state, who))
    assert last_seen >= rounded(sent_at)
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) offline, last seen at the start of that block$/,
          %{who: who},
          %{seen_at: seen_at} = state do
    assert [%{"online" => false, "lastSeen" => last_seen}] = contacts(actor(state, who))
    assert last_seen == seen_at - (@step - 1)
    assert rem(last_seen, @step) == 0
  end

  defstep ~r/^(?<who>\w+) has a contact (?<ghost>\w+) that the hub has not seen since it started$/,
          %{who: who, ghost: ghost_name},
          state do
    # The ghost made its requests before the time that the hub keeps.
    ghost =
      at(now() - @keep - 60, fn ->
        ghost = claimed_node()
        connect(actor(state, who), ghost)
        ghost
      end)

    Sweeper.sweep()
    {:ok, put_actor(state, ghost_name, ghost)}
  end

  defstep ~r/^(?<who>\w+) sees only (?<other>\w+), offline and with no last-seen time$/,
          %{who: who, other: other},
          state do
    other = actor(state, other)

    assert contacts(actor(state, who)) == [
             %{
               "handle" => other.handle,
               "kind" => "personal",
               "endpoint" => other.endpoint,
               "online" => false,
               "lastSeen" => nil
             }
           ]
  end

  defstep ~r/^one node was last seen 7 days and 1 second ago$/, _, _ do
    now = now()
    old = new_node()
    seen_at(old, now - @keep - 1)
    {:ok, %{now: now, old: old}}
  end

  defstep ~r/^another node was last seen 7 days less 1 minute ago$/, _, %{now: now} do
    recent = new_node()
    seen_at(recent, now - @keep + 60)
    {:ok, %{recent: recent}}
  end

  defstep ~r/^the hub forgets the first node$/, _, %{old: old} do
    assert Presence.last_seen(old.endpoint) == nil
  end

  defstep ~r/^the hub still knows when it saw the second node$/, _, %{recent: recent, now: now} do
    assert Presence.last_seen(recent.endpoint) == now - @keep + 60
  end
end
