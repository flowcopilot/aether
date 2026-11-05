defmodule AetherHub.Features.BlocksTest do
  use AetherHub.Feature, file: "blocks.feature", async: true

  import AetherHub.TestClient
  import AetherHub.HubStepsAccounts

  alias AetherHub.{Block, Repo}

  defp block(owner, blocked), do: request(owner, "PUT", "/v1/blocks/#{blocked.endpoint}")
  defp blocks(node), do: json(request(node, "GET", "/v1/blocks"))["blocks"]

  defp invite_id(%{invite_id: id}), do: id

  # Accounts and invites

  defstep ~r/^(?<who>\w+) has a name$/, %{who: who}, state do
    {:ok, remember(state, who, claimed_node())}
  end

  defstep ~r/^(?<one>\w+) and (?<other>\w+) are contacts$/, %{one: one, other: other}, state do
    connect(who(state, one), who(state, other))
  end

  defstep ~r/^(?<who>\w+) invites (?<other>\w+)$/, %{who: who, other: other}, state do
    conn = invite(who(state, who), who(state, other))
    answers = state |> Map.get(:answers, %{}) |> Map.put(who, conn)
    {:ok, %{conn: conn, answers: answers}}
  end

  defstep ~r/^(?<who>\w+) invited (?<other>\w+)$/, %{who: who, other: other}, state do
    conn = invite(who(state, who), who(state, other))
    assert conn.status == 201
    {:ok, %{invite_id: json(conn)["id"]}}
  end

  defstep ~r/^(?<who>\w+) has read her invites$/, %{who: who}, state do
    {:ok, %{version: invites(who(state, who))["version"]}}
  end

  defstep ~r/^(?<who>\w+) has no invites$/, %{who: who}, state do
    assert invites(who(state, who))["invites"] == []
  end

  defstep ~r/^(?<who>\w+) has no contacts$/, %{who: who}, state do
    assert json(request(who(state, who), "GET", "/v1/contacts"))["contacts"] == []
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
    assert [%{"from" => from}] = invites(who(state, who))["invites"]
    assert from == who(state, other).handle
  end

  defstep ~r/^the invite version of (?<who>\w+) moved up by one$/,
          %{who: who},
          %{version: version} = state do
    assert invites(who(state, who))["version"] == version + 1
  end

  defstep ~r/^(?<who>\w+) sees his invite to (?<other>\w+) as its sender$/,
          %{who: who},
          %{answers: answers} = state do
    assert [%{"id" => id, "role" => "sender"}] = invites(who(state, who))["invites"]
    assert id == json(answers[who])["id"]
  end

  defstep ~r/^(?<who>\w+) tries to (?<action>accept|decline|block) the invite$/,
          %{who: who, action: action},
          state do
    node = who(state, who)
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
    {:ok, %{conn: request(who(state, who), "DELETE", "/v1/invites/#{invite_id(state)}")}}
  end

  # Blocks

  defstep ~r/^(?<who>\w+) blocked (?<other>\w+)$/, %{who: who, other: other}, state do
    assert block(who(state, who), who(state, other)).status == 204
  end

  defstep ~r/^(?<who>\w+) blocks (?<target>a key that is bad|a new account|\w+)$/,
          %{who: who, target: target},
          state do
    node = who(state, who)

    conn =
      case target do
        "a key that is bad" -> request(node, "PUT", "/v1/blocks/not-an-endpoint")
        "herself" -> block(node, node)
        "a new account" -> block(node, claimed_node())
        other -> block(node, who(state, other))
      end

    {:ok, %{conn: conn}}
  end

  defstep ~r/^(?<who>\w+) blocks the sender of the invite$/, %{who: who}, state do
    {:ok, %{conn: request(who(state, who), "POST", "/v1/invites/#{invite_id(state)}/block")}}
  end

  defstep ~r/^(?<who>.+) tries to block the sender of (?<invite>the invite|an unknown invite)$/,
          %{who: who, invite: invite},
          state do
    node = if who == "a stranger", do: claimed_node(), else: who(state, who)
    id = if invite == "the invite", do: invite_id(state), else: String.duplicate("0", 32)
    {:ok, %{conn: request(node, "POST", "/v1/invites/#{id}/block")}}
  end

  defstep ~r/^(?<who>\w+) unblocks (?<other>\w+)$/, %{who: who, other: other}, state do
    conn = request(who(state, who), "DELETE", "/v1/blocks/#{who(state, other).endpoint}")
    {:ok, %{conn: conn}}
  end

  defstep ~r/^(?<who>\w+) has blocked the key of (?<other>\w+)$/,
          %{who: who, other: other},
          state do
    assert [%{"endpoint" => endpoint}] = blocks(who(state, who))
    assert endpoint == who(state, other).endpoint
  end

  defstep ~r/^(?<who>\w+) has blocked a key with no handle$/, %{who: who}, state do
    assert [%{"handle" => nil}] = blocks(who(state, who))
  end

  defstep ~r/^the block list of (?<who>\w+) is only (?<other>\w+) with his handle$/,
          %{who: who, other: other},
          state do
    other = who(state, other)
    assert blocks(who(state, who)) == [%{"endpoint" => other.endpoint, "handle" => other.handle}]
  end

  defstep ~r/^(?<who>\w+) has 1,000 blocks$/, %{who: who}, state do
    owner = who(state, who).endpoint

    rows =
      for _ <- 1..1_000 do
        %{owner_endpoint: owner, blocked_endpoint: new_node().endpoint, created_at: 1}
      end

    Repo.insert_all(Block, rows)
  end

  # The blocked account

  defstep ~r/^(?<who>\w+) deletes his account$/, %{who: who}, state do
    assert request(who(state, who), "DELETE", "/v1/account").status == 204
  end

  defstep ~r/^(?<who>\w+) claims his name again$/, %{who: who}, state do
    node = who(state, who)
    assert request(node, "PUT", "/v1/account", %{"name" => node.name}).status == 200
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
end
