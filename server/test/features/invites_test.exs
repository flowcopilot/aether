defmodule AetherHub.Features.InvitesTest do
  use AetherHub.Feature, file: "invites.feature", async: true

  alias AetherHub.{Contract, Invite, Repo}

  @prefix Contract.get(["limits", "invitePrefix"])
  @max_bundle Contract.get(["limits", "maxInviteBytes"])
  @bundle @prefix <> "signed-line"

  defp list(node, query \\ "") do
    path = if query == "", do: "/v1/invites", else: "/v1/invites" <> query
    conn = http_get(node, path)
    assert conn.status == 200
    json(conn)
  end

  defp version(node), do: list(node)["version"]

  defp make_invite(state, from, to, days) do
    conn =
      request(actor(state, from), "POST", "/v1/invites", %{
        "to" => actor(state, to).handle,
        "days" => days
      })

    assert conn.status == 201
    %{conn: conn, invite: json(conn), old_invite: state[:invite]}
  end

  defp id(state), do: state.invite["id"]

  # The only invite on the list of `who`, which must be the scenario's invite.
  defp seen(state, who) do
    assert [%{"id" => id} = invite] = list(actor(state, who))["invites"]
    assert id == id(state)
    invite
  end

  defp act(state, who, "accepts the invite"),
    do: request(actor(state, who), "POST", "/v1/invites/#{id(state)}/accept")

  defp act(state, who, "deletes the invite"),
    do: request(actor(state, who), "DELETE", "/v1/invites/#{id(state)}")

  defp act(state, who, "sends the bundle"), do: send_bundle(state, who, @bundle)

  defp send_bundle(state, who, bundle),
    do:
      request(actor(state, who), "POST", "/v1/invites/#{id(state)}/bundle", %{"bundle" => bundle})

  defp target(state, from, "herself"), do: actor(state, from).handle
  defp target(_state, _from, "an unknown name"), do: unique_name() <> "#0001"

  defp target(state, _from, text) do
    case Regex.run(~r/^(\w+)'s name (without the number|with the number (\d+))$/, text) do
      [_, name, "without the number"] -> actor(state, name).name
      [_, name, _, number] -> actor(state, name).name <> "#" <> number
      nil -> actor(state, text).handle
    end
  end

  defp days(text) do
    case Regex.run(~r/^(\d+) days?$/, text) do
      [_, n] -> %{"days" => String.to_integer(n)}
      nil when text == "no days" -> %{}
      nil -> %{"days" => List.last(Regex.run(~r/^the text "(.*)"$/, text))}
    end
  end

  defp bundle("a bundle without the invite prefix"), do: "not-an-invite"

  defp bundle("a bundle with 8 KiB of text after the prefix"),
    do: @prefix <> String.duplicate("a", @max_bundle)

  defp bundle("no bundle"), do: nil

  defp bundle("a bundle of exactly 8 KiB with the prefix"),
    do: @prefix <> String.duplicate("a", @max_bundle - byte_size(@prefix))

  defp by_handle(state, handle) do
    {name, _node} = Enum.find(state.actors, fn {_name, node} -> node[:handle] == handle end)
    name
  end

  # Making and moving invites

  defstep ~r/^(?<from>\w+) invites (?<to>\w+) for (?<days>\d+) days$/,
          %{from: from, to: to, days: days},
          state do
    {:ok, make_invite(state, from, to, String.to_integer(days))}
  end

  defstep ~r/^(?<from>\w+) invited (?<to>\w+)(?: for (?<days>\d+) days)?$/,
          %{from: from, to: to, days: days},
          state do
    days = if days == "", do: 7, else: String.to_integer(days)
    {:ok, make_invite(state, from, to, days)}
  end

  defstep ~r/^(?<from>\w+) sends an invite to (?<to>.+?) for (?<days>.+)$/,
          %{from: from, to: to, days: days},
          state do
    body = Map.merge(%{"to" => target(state, from, to)}, days(days))
    {:ok, %{conn: request(actor(state, from), "POST", "/v1/invites", body)}}
  end

  defstep ~r/^the invite is (?<stage>pending|accepted|delivered)$/, %{stage: stage}, state do
    if stage in ["accepted", "delivered"] do
      receiver = by_handle(state, state.invite["to"])
      assert act(state, receiver, "accepts the invite").status == 200
    end

    if stage == "delivered" do
      sender = by_handle(state, state.invite["from"])
      assert act(state, sender, "sends the bundle").status == 200
    end
  end

  defstep ~r/^(?<who>\w+) (?<action>accepts the invite|deletes the invite|sends the bundle)$/,
          %{who: who, action: action},
          state do
    {:ok, %{conn: act(state, who, action)}}
  end

  defstep ~r/^(?<who>\w+) sends (?<bundle>a bundle .+|no bundle)$/,
          %{who: who, bundle: text},
          state do
    {:ok, %{conn: send_bundle(state, who, bundle(text))}}
  end

  defstep ~r/^(?<who>\w+) accepts the old invite$/, %{who: who}, state do
    {:ok,
     %{conn: request(actor(state, who), "POST", "/v1/invites/#{state.old_invite["id"]}/accept")}}
  end

  defstep ~r/^(?<who>\w+) accepts an invite that does not exist$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "POST", "/v1/invites/nope/accept")}}
  end

  defstep ~r/^(?<who>\w+) lists (?:her|his|its) invites$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "GET", "/v1/invites")}}
  end

  defstep ~r/^(?<who>\w+) lists (?:her|his|its) invites with (?<parameter>\S+)$/,
          %{who: who, parameter: parameter},
          state do
    {:ok, %{conn: request(actor(state, who), "GET", "/v1/invites?" <> parameter)}}
  end

  # What each side sees

  defstep ~r/^the answer is the invite as the (?<role>sender|receiver), (?<status>\w+)$/,
          %{role: role, status: status},
          %{conn: conn} do
    invite = json(conn)
    assert %{"role" => ^role, "status" => ^status} = invite
    {:ok, %{it: invite}}
  end

  defstep ~r/^(?<who>\w+) sees the invite as the (?<role>sender|receiver), (?<status>\w+)$/,
          %{who: who, role: role, status: status},
          state do
    invite = seen(state, who)
    assert %{"role" => ^role, "status" => ^status} = invite
    {:ok, %{it: invite}}
  end

  defstep ~r/^it is from (?<from>\w+) to (?<to>\w+) for (?<days>\d+) days$/,
          %{from: from, to: to, days: days},
          %{it: it} = state do
    assert {it["from"], it["to"]} == {actor(state, from).handle, actor(state, to).handle}
    assert it["days"] == String.to_integer(days)
  end

  defstep ~r/^it shows when it was made$/, _, %{it: it} do
    assert is_integer(it["createdAt"])
  end

  defstep ~r/^it shows no endpoint$/, _, %{it: it} do
    assert {it["senderEndpoint"], it["receiverEndpoint"]} == {nil, nil}
  end

  # The sender sees the receiver's endpoint, and the receiver the sender's.
  defstep ~r/^it shows (?<other>\w+)'s endpoint$/, %{other: other}, %{it: it} = state do
    endpoint = actor(state, other).endpoint

    case it["role"] do
      "sender" -> assert {it["receiverEndpoint"], it["senderEndpoint"]} == {endpoint, nil}
      "receiver" -> assert {it["senderEndpoint"], it["receiverEndpoint"]} == {endpoint, nil}
    end
  end

  defstep ~r/^it shows no bundle$/, _, %{it: it} do
    assert it["bundle"] == nil
  end

  defstep ~r/^it shows the bundle$/, _, %{it: it} do
    assert it["bundle"] == @bundle
  end

  defstep ~r/^(?<who>\w+) sees no invites$/, %{who: who}, state do
    assert list(actor(state, who))["invites"] == []
  end

  defstep ~r/^the new invite is not the old invite$/, _, state do
    assert state.invite["id"] != state.old_invite["id"]
  end

  defstep ~r/^(?<who>\w+) sees only the new invite, for (?<days>\d+) days$/,
          %{who: who, days: days},
          state do
    days = String.to_integer(days)
    id = id(state)
    assert [%{"id" => ^id, "days" => ^days}] = list(actor(state, who))["invites"]
  end

  defstep ~r/^(?<who>\w+) sees one invite, from (?<from>\w+)$/, %{who: who, from: from}, state do
    handle = actor(state, from).handle
    assert [%{"from" => ^handle}] = list(actor(state, who))["invites"]
  end

  # Versions

  defstep ~r/^the versions? of (?<names>\w+(?: and \w+)*) (?:is|are) noted$/,
          %{names: names},
          state do
    noted = for name <- split_names(names), into: %{}, do: {name, version(actor(state, name))}
    {:ok, %{versions: Map.merge(state[:versions] || %{}, noted)}}
  end

  defstep ~r/^the versions? of (?<names>\w+(?: and \w+)*) (?:is|are) (?<n>\d+) higher$/,
          %{names: names, n: n},
          state do
    for name <- split_names(names) do
      assert version(actor(state, name)) == state.versions[name] + String.to_integer(n)
    end
  end

  # Expiry

  defstep ~r/^the hub keeps no record of the invite$/, _, state do
    assert Repo.get(Invite, id(state)) == nil
  end

  # Waiting for a change

  defstep ~r/^(?<who>\w+) starts to wait up to (?<seconds>\d+) seconds for a change after that version$/,
          %{who: who, seconds: seconds},
          state do
    node = actor(state, who)
    query = "?after=#{state.versions[who]}&wait=#{seconds}"
    started = System.monotonic_time(:millisecond)
    task = Task.async(fn -> list(node, query) end)
    Process.sleep(200)
    {:ok, %{waiter: who, wait: task, started: started}}
  end

  defstep ~r/^(?<who>\w+) waits up to (?<seconds>\d+) seconds? for a change after (?<base>that|an older) version$/,
          %{who: who, seconds: seconds, base: base},
          state do
    v = if base == "that", do: state.versions[who], else: state.versions[who] - 1
    started = System.monotonic_time(:millisecond)
    result = list(actor(state, who), "?after=#{v}&wait=#{seconds}")
    elapsed = System.monotonic_time(:millisecond) - started
    {:ok, %{waiter: who, result: result, elapsed: elapsed}}
  end

  defstep ~r/^the wait ends with the version 1 higher and the new invite for (?<who>\w+) as receiver$/,
          %{who: who},
          state do
    result = Task.await(state.wait, 15_000)
    elapsed = System.monotonic_time(:millisecond) - state.started
    assert result["version"] == state.versions[who] + 1
    assert [%{"role" => "receiver"}] = result["invites"]
    {:ok, %{result: result, elapsed: elapsed}}
  end

  defstep ~r/^the wait ends with the same version and no invites$/, _, state do
    assert state.result == %{"version" => state.versions[state.waiter], "invites" => []}
  end

  defstep ~r/^the wait ends with the noted version$/, _, state do
    assert state.result["version"] == state.versions[state.waiter]
  end

  defstep ~r/^the wait took less than (?<seconds>\d+) seconds?$/, %{seconds: s}, state do
    assert state.elapsed < String.to_integer(s) * 1_000
  end

  defstep ~r/^the wait took at least (?<seconds>\d+) seconds?$/, %{seconds: s}, state do
    assert state.elapsed >= String.to_integer(s) * 1_000
  end
end
