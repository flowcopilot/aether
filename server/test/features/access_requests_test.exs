defmodule AetherHub.Features.AccessRequestsTest do
  use AetherHub.Feature, file: "access-requests.feature", async: true

  alias AetherHub.Clock

  defp ask(requester, service, days \\ 7) do
    request(requester, "POST", "/v1/requests", %{"to" => service.handle, "days" => days})
  end

  defp requests(node), do: invites(node)["invites"]

  defp approve(node, id, body),
    do: request(node, "POST", "/v1/invites/#{id}/approve", body)

  defp id(state), do: state.last_request["id"]

  defp target(state, "a person"), do: {state, claimed_node()}
  defp target(state, "an unlisted service agent"), do: {state, claimed_node("service")}
  defp target(state, "one more service agent"), do: {state, listed_service()}
  defp target(state, "an unknown name"), do: {state, %{handle: unique_name() <> "#0001"}}
  defp target(state, "a name without a number"), do: {state, %{handle: "no-number"}}

  # A listing of one day, published two days ago.
  defp target(state, "a service agent whose listing expired") do
    expired =
      at(Clock.now() - 2 * 86_400, fn ->
        service = claimed_node("service")
        %{status: 200} = put_listing(service, listing(service), expires_at: Clock.now() + 86_400)
        service
      end)

    {state, expired}
  end

  defp target(state, name), do: {state, actor(state, name)}

  defp answered(state, who, conn) do
    answer = if conn.status == 201, do: json(conn)
    answers = Map.put(state[:answers] || %{}, who, answer)
    changes = %{conn: conn, answers: answers}
    if answer, do: Map.put(changes, :last_request, answer), else: changes
  end

  # Asking

  defstep ~r/^(?<who>\w+) asks (?<target>.+?) for access(?: for (?<days>\d+) days| (?<text>with a text))?$/,
          %{who: who, target: target, days: days, text: text},
          state do
    requester = actor(state, who)
    {state, service} = target(state, target)

    conn =
      cond do
        text != "" ->
          body = %{"to" => service.handle, "days" => 7, "text" => "please"}
          request(requester, "POST", "/v1/requests", body)

        days != "" ->
          ask(requester, service, String.to_integer(days))

        true ->
          ask(requester, service)
      end

    {:ok, answered(state, who, conn)}
  end

  defstep ~r/^(?<who>\w+) asked (?<service>\w+) for access$/,
          %{who: who, service: service},
          state do
    conn = ask(actor(state, who), actor(state, service))
    assert conn.status == 201
    {:ok, Map.put(answered(state, who, conn), :request, json(conn))}
  end

  defstep ~r/^(?<who>\w+) has open requests to 5 service agents$/, %{who: who}, state do
    requester = actor(state, who)
    statuses = for _ <- 1..5, do: ask(requester, listed_service()).status
    assert Enum.uniq(statuses) == [201]
  end

  defstep ~r/^(?<who>\w+) asked (?<service>\w+) for access and deleted the request 10 times$/,
          %{who: who, service: service},
          state do
    requester = actor(state, who)

    for _ <- 1..10 do
      conn = ask(requester, actor(state, service))
      assert conn.status == 201
      assert request(requester, "DELETE", "/v1/invites/#{json(conn)["id"]}").status == 204
    end
  end

  defstep ~r/^52 people with accounts of different ages asked (?<service>\w+) for access$/,
          %{service: service},
          state do
    # One account a second, the oldest first.
    start = Clock.now() - 60
    requesters = for index <- 1..52, do: at(start + index, &claimed_node/0)

    for requester <- requesters do
      %{status: 201} = ask(requester, actor(state, service))
    end

    {:ok, %{oldest: hd(requesters), newest: List.last(requesters)}}
  end

  # Approving and delivering

  defstep ~r/^(?<who>a stranger|\w+) approves the request(?: for (?<days>\d+) days| (?<text>with a text))?$/,
          %{who: who, days: days, text: text},
          state do
    node = if who == "a stranger", do: claimed_node(), else: actor(state, who)

    body =
      cond do
        text != "" -> %{"days" => 3, "text" => "x"}
        days != "" -> %{"days" => String.to_integer(days)}
        true -> nil
      end

    {:ok, %{conn: approve(node, id(state), body)}}
  end

  defstep ~r/^(?<who>\w+) accepts the request$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "POST", "/v1/invites/#{id(state)}/accept")}}
  end

  defstep ~r/^(?<who>\w+) sends the bundle "(?<bundle>[^"]+)"$/,
          %{who: who, bundle: bundle},
          state do
    path = "/v1/invites/#{id(state)}/bundle"
    {:ok, %{conn: request(actor(state, who), "POST", path, %{"bundle" => bundle})}}
  end

  # Blocks

  defstep ~r/^(?<who>\w+) blocks through the request$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "POST", "/v1/invites/#{id(state)}/block")}}
  end

  # Answers

  defstep ~r/^the answer is a request from (?<from>\w+) to (?<to>\w+), for \k<to> as receiver, without endpoints$/,
          %{from: from, to: to},
          %{conn: conn} = state do
    assert %{
             "status" => "requested",
             "origin" => "request",
             "role" => "receiver",
             "from" => sender,
             "to" => receiver,
             "receiverEndpoint" => nil,
             "senderEndpoint" => nil
           } = json(conn)

    assert is_binary(json(conn)["id"])
    assert {sender, receiver} == {actor(state, from).handle, actor(state, to).handle}
  end

  defstep ~r/^the answer is the request accepted for (?<days>\d+) days$/,
          %{days: days},
          %{conn: conn} do
    days = String.to_integer(days)
    assert %{"status" => "accepted", "days" => ^days} = json(conn)
  end

  defstep ~r/^the hub answers 201 with the same request$/, _, %{conn: conn} = state do
    assert conn.status == 201
    assert json(conn) == state.request
  end

  defstep ~r/^the hub answers 201 with a new request$/, _, %{conn: conn} = state do
    assert conn.status == 201
    assert json(conn)["id"] != state.request["id"]
  end

  defstep ~r/^the answers to (?<one>\w+) and (?<other>\w+) have the same fields$/,
          %{one: one, other: other},
          state do
    assert Map.keys(state.answers[one]) == Map.keys(state.answers[other])
  end

  # What each side sees

  defstep ~r/^(?<who>\w+) sees the request as the sender, with the endpoint of (?<other>\w+)$/,
          %{who: who, other: other},
          state do
    id = id(state)

    assert [%{"id" => ^id, "role" => "sender", "receiverEndpoint" => endpoint}] =
             requests(actor(state, who))

    assert endpoint == actor(state, other).endpoint
  end

  defstep ~r/^(?<who>\w+) sees the request as the receiver, requested$/, %{who: who}, state do
    id = state.answers[who]["id"]

    assert [%{"id" => ^id, "role" => "receiver", "status" => "requested"}] =
             requests(actor(state, who))
  end

  defstep ~r/^(?<who>\w+) sees the request delivered with the bundle "(?<bundle>[^"]+)" and the endpoint of (?<service>\w+)$/,
          %{who: who, bundle: bundle, service: service},
          state do
    assert [%{"status" => "delivered", "bundle" => ^bundle, "senderEndpoint" => sender}] =
             requests(actor(state, who))

    assert sender == actor(state, service).endpoint
  end

  defstep ~r/^(?<who>\w+) sees no requests$/, %{who: who}, state do
    assert requests(actor(state, who)) == []
  end

  defstep ~r/^(?<who>\w+) sees one request$/, %{who: who}, state do
    assert length(requests(actor(state, who))) == 1
  end

  defstep ~r/^(?<who>\w+) sees only the request of (?<other>\w+)$/,
          %{who: who, other: other},
          state do
    assert [%{"id" => id}] = requests(actor(state, who))
    assert id == state.answers[other]["id"]
  end

  defstep ~r/^(?<who>\w+) sees no request from (?<other>\w+)$/,
          %{who: who, other: other},
          state do
    handle = actor(state, other).handle
    refute Enum.any?(requests(actor(state, who)), &(&1["to"] == handle))
  end

  defstep ~r/^(?<who>\w+) sees 50 requests$/, %{who: who}, state do
    assert length(requests(actor(state, who))) == 50
  end

  defstep ~r/^(?<who>\w+) sees the request of the oldest account$/, %{who: who}, state do
    handles = MapSet.new(requests(actor(state, who)), & &1["to"])
    assert MapSet.member?(handles, state.oldest.handle)
  end

  defstep ~r/^(?<who>\w+) does not see the request of the newest account$/, %{who: who}, state do
    handles = MapSet.new(requests(actor(state, who)), & &1["to"])
    refute MapSet.member?(handles, state.newest.handle)
  end

  defstep ~r/^the newest account sees its request as requested$/, _, state do
    assert [%{"status" => "requested"}] = requests(state.newest)
  end

  # Contacts

  defstep ~r/^(?<who>\w+) has (?<other>\w+) as (?:her|his|its) only contact$/,
          %{who: who, other: other},
          state do
    assert [%{"handle" => handle}] =
             json(request(actor(state, who), "GET", "/v1/contacts"))["contacts"]

    assert handle == actor(state, other).handle
  end

  # Versions and expiry

  defstep ~r/^the version of (?<who>\w+) is noted$/, %{who: who}, state do
    {:ok, %{version: invites(actor(state, who))["version"]}}
  end

  defstep ~r/^the version of (?<who>\w+) is the same$/, %{who: who}, state do
    assert invites(actor(state, who))["version"] == state.version
  end
end
