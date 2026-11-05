defmodule AetherHub.Features.ContactsTest do
  use AetherHub.Feature, file: "contacts.feature", async: true

  import AetherHub.TestClient
  import AetherHub.HubStepsAccounts

  alias AetherHub.{Account, Contact, Presence, Repo}

  defp now, do: System.system_time(:second)
  defp rounded(time), do: time - rem(time, 900)

  # Accounts

  defstep ~r/^(?<who>\w+) has a name$/, %{who: who}, state do
    {:ok, remember(state, who, claimed_node())}
  end

  defstep ~r/^(?<who>\w+) is a new node$/, %{who: who}, state do
    {:ok, remember(state, who, new_node())}
  end

  defstep ~r/^(?<first>\w+) and (?<second>\w+) have names, and the handle of \k<first> sorts first$/,
          %{first: first, second: second},
          state do
    [one, two] = Enum.sort_by([claimed_node(), claimed_node()], & &1.handle)
    {:ok, state |> remember(first, one) |> remember(second, two)}
  end

  defstep ~r/^(?<who>\w+) changes his name$/, %{who: who}, state do
    conn = request(who(state, who), "PUT", "/v1/account", %{"name" => unique_name()})
    assert conn.status == 200
    {:ok, %{new_handle: json(conn)["handle"]}}
  end

  # Invites and contacts

  defstep ~r/^(?<one>\w+) and (?<other>\w+) are contacts$/, %{one: one, other: other}, state do
    {:ok, %{invite_id: connect(who(state, one), who(state, other))}}
  end

  defstep ~r/^(?<one>\w+) and (?<other>\w+) become contacts again with a new invite$/,
          %{one: one, other: other},
          state do
    connect(who(state, one), who(state, other))
  end

  defstep ~r/^(?<who>\w+) invites (?<other>\w+)$/, %{who: who, other: other}, state do
    {:ok, %{invite_id: json(invite(who(state, who), who(state, other)))["id"]}}
  end

  defstep ~r/^(?<who>\w+) accepts the invite$/, %{who: who}, %{invite_id: id} = state do
    assert request(who(state, who), "POST", "/v1/invites/#{id}/accept").status == 200
  end

  defstep ~r/^(?<who>\w+) deletes the invite$/, %{who: who}, %{invite_id: id} = state do
    {:ok, %{conn: request(who(state, who), "DELETE", "/v1/invites/#{id}")}}
  end

  defstep ~r/^(?<who>\w+) has only (?<other>\w+) as a contact$/,
          %{who: who, other: other},
          state do
    other = who(state, other)
    assert [%{"handle" => handle, "endpoint" => endpoint}] = contacts(who(state, who))
    assert {handle, endpoint} == {other.handle, other.endpoint}
  end

  defstep ~r/^(?<who>\w+) has (?<count>\d+) contacts?$/, %{who: who, count: count}, state do
    assert length(contacts(who(state, who))) == String.to_integer(count)
  end

  defstep ~r/^(?<who>\w+) has no contacts$/, %{who: who}, state do
    assert contacts(who(state, who)) == []
  end

  defstep ~r/^(?<who>\w+) sees her contacts in this order: (?<names>.+)$/,
          %{who: who, names: names},
          state do
    expected = names |> String.split(", ") |> Enum.map(&who(state, &1).handle)
    assert Enum.map(contacts(who(state, who)), & &1["handle"]) == expected
  end

  defstep ~r/^(?<who>\w+) sees the new handle of (?<other>\w+), as a personal agent$/,
          %{who: who},
          %{new_handle: new_handle} = state do
    assert [%{"handle" => ^new_handle, "kind" => "personal"}] = contacts(who(state, who))
  end

  defstep ~r/^(?<who>\w+) removes (?<other>\w+) from (?:her|his) contacts$/,
          %{who: who, other: other},
          state do
    path = "/v1/contacts/#{who(state, other).endpoint}"
    {:ok, %{conn: request(who(state, who), "DELETE", path)}}
  end

  defstep ~r/^(?<who>\w+) reads his contacts$/, %{who: who}, state do
    {:ok, %{conn: request(who(state, who), "GET", "/v1/contacts")}}
  end

  # Presence

  defstep ~r/^(?<who>\w+) was last seen (?<seconds>\d+) seconds ago$/,
          %{who: who, seconds: seconds},
          state do
    seen_at = now() - String.to_integer(seconds)
    Presence.seen(who(state, who).endpoint, seen_at)
    {:ok, %{seen_at: seen_at}}
  end

  defstep ~r/^(?<who>\w+) was last seen long ago$/, %{who: who}, state do
    Presence.seen(who(state, who).endpoint, 1)
  end

  defstep ~r/^(?<who>\w+) was last seen one hour ago, in the last second of a 15-minute block$/,
          %{who: who},
          state do
    seen = now() - 3_600
    seen = seen - rem(seen, 900) + 899
    Presence.seen(who(state, who).endpoint, seen)
    {:ok, %{seen_at: seen}}
  end

  defstep ~r/^(?<who>\w+) sends a signed request$/, %{who: who}, state do
    sent_at = now()
    request(who(state, who), "GET", "/v1/account")
    {:ok, %{sent_at: sent_at}}
  end

  defstep ~r/^(?<who>\w+) waits for news for (?<seconds>\d+) seconds$/,
          %{who: who, seconds: seconds},
          %{seen_at: seen_at} = state do
    node = who(state, who)
    version = invites(node)["version"]
    Presence.seen(node.endpoint, seen_at)

    task =
      Task.async(fn ->
        request(node, "GET", "/v1/invites?after=#{version}&wait=#{seconds}")
      end)

    Process.sleep(200)
    # The poll's own request sets the last-seen time; push it back to
    # isolate the poll.
    Presence.seen(node.endpoint, seen_at)
    {:ok, %{wait: task}}
  end

  defstep ~r/^the wait of (?<who>\w+) ends$/, _, %{wait: task} do
    assert Task.await(task, 5_000).status == 200
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) (?<presence>online|offline)$/,
          %{who: who, presence: presence},
          state do
    online = presence == "online"
    assert [%{"online" => ^online}] = contacts(who(state, who))
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) (?<presence>online|offline), last seen (?<seconds>\d+) seconds ago rounded down to 15 minutes$/,
          %{who: who, presence: presence},
          %{seen_at: seen_at} = state do
    online = presence == "online"
    assert [%{"online" => ^online, "lastSeen" => last_seen}] = contacts(who(state, who))
    assert last_seen == rounded(seen_at)
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) online, last seen now rounded down to 15 minutes$/,
          %{who: who},
          %{sent_at: sent_at} = state do
    assert [%{"online" => true, "lastSeen" => last_seen}] = contacts(who(state, who))
    assert last_seen >= rounded(sent_at)
  end

  defstep ~r/^(?<who>\w+) sees (?<other>\w+) offline, last seen at the start of that block$/,
          %{who: who},
          %{seen_at: seen_at} = state do
    assert [%{"online" => false, "lastSeen" => last_seen}] = contacts(who(state, who))
    assert last_seen == seen_at - 899
    assert rem(last_seen, 900) == 0
  end

  defstep ~r/^(?<who>\w+) has read her invites$/, %{who: who}, state do
    {:ok, %{version: invites(who(state, who))["version"]}}
  end

  defstep ~r/^the invite version of (?<who>\w+) did not move$/,
          %{who: who},
          %{version: version} = state do
    assert invites(who(state, who))["version"] == version
  end

  defstep ~r/^(?<who>\w+) has a contact (?<ghost>\w+) that the hub has not seen since it started$/,
          %{who: who, ghost: ghost_name},
          state do
    owner = who(state, who)
    ghost = new_node()
    name = unique_name()
    Repo.insert!(%Account{endpoint: ghost.endpoint, name: name, number: 7, version: 1})

    Repo.insert_all(Contact, [
      %{owner_endpoint: owner.endpoint, contact_endpoint: ghost.endpoint},
      %{owner_endpoint: ghost.endpoint, contact_endpoint: owner.endpoint}
    ])

    {:ok, remember(state, ghost_name, Map.put(ghost, :handle, name <> "#0007"))}
  end

  defstep ~r/^(?<who>\w+) sees only (?<other>\w+), offline and with no last-seen time$/,
          %{who: who, other: other},
          state do
    other = who(state, other)

    assert contacts(who(state, who)) == [
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
    Presence.seen(old.endpoint, now - 7 * 86_400 - 1)
    {:ok, %{now: now, old: old}}
  end

  defstep ~r/^another node was last seen 7 days less 1 minute ago$/, _, %{now: now} do
    recent = new_node()
    Presence.seen(recent.endpoint, now - 7 * 86_400 + 60)
    {:ok, %{recent: recent}}
  end

  defstep ~r/^the hub cleans up presence$/, _, %{now: now} do
    Presence.sweep(now)
  end

  defstep ~r/^the hub forgets the first node$/, _, %{old: old} do
    assert Presence.last_seen(old.endpoint) == nil
  end

  defstep ~r/^the hub still knows when it saw the second node$/, _, %{recent: recent, now: now} do
    assert Presence.last_seen(recent.endpoint) == now - 7 * 86_400 + 60
  end

  # The answer

  defstep ~r/^the hub answers (?<status>\d+)$/, %{status: status}, %{conn: conn} do
    assert conn.status == String.to_integer(status)
  end

  defstep ~r/^the hub answers (?<status>\d+) with the error "(?<error>\w+)"$/,
          %{status: status, error: error},
          %{conn: conn} do
    assert conn.status == String.to_integer(status)
    assert json(conn) == %{"error" => error}
  end

  defstep ~r/^the hub answers with the error "(?<error>\w+)"$/, %{error: error}, %{conn: conn} do
    assert json(conn) == %{"error" => error}
  end
end
