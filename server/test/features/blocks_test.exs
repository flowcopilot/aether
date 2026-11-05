defmodule AetherHub.Features.BlocksTest do
  use AetherHub.Feature, file: "blocks.feature", async: true

  alias AetherHub.Clock

  defp blocks(node), do: json(request(node, "GET", "/v1/blocks"))["blocks"]

  defp invite_id(%{invite_id: id}), do: id

  defstep ~r/^(?<who>\w+) invites (?<other>\w+)$/, %{who: who, other: other}, state do
    conn = invite(actor(state, who), actor(state, other))
    answers = state |> Map.get(:answers, %{}) |> Map.put(who, conn)
    {:ok, %{conn: conn, answers: answers}}
  end

  defstep ~r/^(?<who>\w+) invited (?<other>\w+)$/, %{who: who, other: other}, state do
    conn = invite(actor(state, who), actor(state, other))
    assert conn.status == 201
    {:ok, %{invite_id: json(conn)["id"]}}
  end

  defstep ~r/^(?<who>\w+) has no invites$/, %{who: who}, state do
    assert invites(actor(state, who))["invites"] == []
  end

  defstep ~r/^the answer to (?<blocked>\w+) looks like the answer to (?<other>\w+)$/,
          %{blocked: blocked, other: other},
          %{answers: answers} do
    assert answers[blocked].status == 201
    assert Map.keys(json(answers[blocked])) == Map.keys(json(answers[other]))
  end

  defstep ~r/^the invite of (?<who>\w+) is pending$/, %{who: who}, %{answers: answers} do
    assert json(answers[who])["status"] == "pending"
  end

  defstep ~r/^(?<who>\w+) sees only the invite from (?<other>\w+)$/,
          %{who: who, other: other},
          state do
    assert [%{"from" => from}] = invites(actor(state, who))["invites"]
    assert from == actor(state, other).handle
  end

  defstep ~r/^the invite version of (?<who>\w+) moved up by one$/,
          %{who: who},
          %{version: version} = state do
    assert invites(actor(state, who))["version"] == version + 1
  end

  defstep ~r/^(?<who>\w+) sees his invite to (?<other>\w+) as its sender$/,
          %{who: who},
          %{answers: answers} = state do
    assert [%{"id" => id, "role" => "sender"}] = invites(actor(state, who))["invites"]
    assert id == json(answers[who])["id"]
  end

  defstep ~r/^(?<who>\w+) tries to (?<action>accept|decline|block) the invite$/,
          %{who: who, action: action},
          state do
    node = actor(state, who)
    id = invite_id(state)

    conn =
      case action do
        "accept" -> request(node, "POST", "/v1/invites/#{id}/accept")
        "decline" -> request(node, "DELETE", "/v1/invites/#{id}")
        "block" -> request(node, "POST", "/v1/invites/#{id}/block")
      end

    {:ok, %{conn: conn}}
  end

  defstep ~r/^(?<who>\w+) cancels the invite$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "DELETE", "/v1/invites/#{invite_id(state)}")}}
  end

  # Blocks

  defstep ~r/^(?<who>\w+) blocks (?<target>a key that is bad|a new account)$/,
          %{who: who, target: target},
          state do
    node = actor(state, who)

    conn =
      case target do
        "a key that is bad" -> request(node, "PUT", "/v1/blocks/not-an-endpoint")
        "a new account" -> block(node, claimed_node())
      end

    {:ok, %{conn: conn}}
  end

  defstep ~r/^(?<who>\w+) blocks the sender of the invite$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "POST", "/v1/invites/#{invite_id(state)}/block")}}
  end

  defstep ~r/^(?<who>.+) tries to block the sender of (?<invite>the invite|an unknown invite)$/,
          %{who: who, invite: invite},
          state do
    node = if who == "a stranger", do: claimed_node(), else: actor(state, who)
    id = if invite == "the invite", do: invite_id(state), else: String.duplicate("0", 32)
    {:ok, %{conn: request(node, "POST", "/v1/invites/#{id}/block")}}
  end

  defstep ~r/^(?<who>\w+) unblocks (?<other>\w+)$/, %{who: who, other: other}, state do
    conn = request(actor(state, who), "DELETE", "/v1/blocks/#{actor(state, other).endpoint}")
    {:ok, %{conn: conn}}
  end

  defstep ~r/^(?<who>\w+) has blocked the key of (?<other>\w+)$/,
          %{who: who, other: other},
          state do
    assert [%{"endpoint" => endpoint}] = blocks(actor(state, who))
    assert endpoint == actor(state, other).endpoint
  end

  defstep ~r/^(?<who>\w+) has blocked a key with no handle$/, %{who: who}, state do
    assert [%{"handle" => nil}] = blocks(actor(state, who))
  end

  defstep ~r/^the block list of (?<who>\w+) is only (?<other>\w+) with his handle$/,
          %{who: who, other: other},
          state do
    other = actor(state, other)

    assert blocks(actor(state, who)) == [
             %{"endpoint" => other.endpoint, "handle" => other.handle}
           ]
  end

  # A node sends at most 120 requests a minute, so the blocks take a minute
  # of the clock for each 100.
  defstep ~r/^(?<who>\w+) has 1,000 blocks$/, %{who: who}, state do
    owner = actor(state, who)
    start = Clock.now() - 10 * 60

    for minute <- 0..9 do
      at(start + minute * 60, fn ->
        for _ <- 1..100, do: assert(block(owner, new_node()).status == 204)
      end)
    end
  end

  # The blocked account

  defstep ~r/^(?<who>\w+) claims his name again$/, %{who: who}, state do
    node = actor(state, who)
    assert request(node, "PUT", "/v1/account", %{"name" => node.name}).status == 200
  end
end
