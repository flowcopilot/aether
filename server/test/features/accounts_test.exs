defmodule AetherHub.Features.AccountsTest do
  use AetherHub.Feature, file: "accounts.feature", async: true

  alias AetherHub.{Account, Contract, Names, Repo}

  defp put(node, body), do: request(node, "PUT", "/v1/account", body)

  # The name that a step calls "a new name" (a fresh one) or "that name"
  # (the one an earlier step chose), with the state change.
  defp new_name(_state, "a new name") do
    name = unique_name()
    {name, %{new_name: name}}
  end

  defp new_name(%{new_name: name}, "that name"), do: {name, %{}}

  defstep ~r/^(?<who>\w+) has no account$/, %{who: who}, state do
    assert request(actor(state, who), "GET", "/v1/account").status == 404
  end

  # Claims

  defstep ~r/^(?<who>\w+) claims a new name$/, %{who: who}, state do
    name = unique_name()
    {:ok, %{new_name: name, conn: put(actor(state, who), %{"name" => name})}}
  end

  defstep ~r/^(?<who>\w+) claims a new service name that is searchable$/, %{who: who}, state do
    body = %{"name" => unique_name(), "kind" => "service", "searchable" => true}
    {:ok, %{conn: put(actor(state, who), body)}}
  end

  defstep ~r/^(?<who>\w+) claims (?:her|his) name again(?: (?<type>as a personal agent|as a service agent|without a type))?$/,
          %{who: who, type: type},
          state do
    node = actor(state, who)

    body =
      case type do
        "as a personal agent" -> %{"name" => node.name, "kind" => "personal"}
        "as a service agent" -> %{"name" => node.name, "kind" => "service"}
        _ -> %{"name" => node.name}
      end

    {:ok, %{conn: put(node, body)}}
  end

  defstep ~r/^two new nodes claim the same name$/, _, _ do
    name = unique_name()
    {:ok, %{claims: for(_ <- 1..2, do: json(put(new_node(), %{"name" => name})))}}
  end

  defstep ~r/^they get different numbers$/, _, %{claims: [first, second]} do
    assert first["name"] == second["name"]
    assert first["number"] != second["number"]
  end

  defstep ~r/^a new node claims the name (?<name>.+)$/, %{name: name}, _ do
    {:ok, %{conn: put(new_node(), %{"name" => JSON.decode!(name)})}}
  end

  defstep ~r/^a new node sends a claim with (?<content>.+)$/, %{content: content}, _ do
    body =
      case content do
        "a handle instead of a name" -> %{"handle" => unique_name()}
        "the unknown type robot" -> %{"name" => unique_name(), "kind" => "robot"}
        "no fields" -> %{}
        "a body that is not JSON" -> "not json"
        "a JSON list" -> "[]"
      end

    {:ok, %{conn: put(new_node(), body)}}
  end

  # Renames and other changes

  defstep ~r/^(?<who>\w+) renames herself to (?<which>a new name|that name)$/,
          %{who: who, which: which},
          state do
    {name, changes} = new_name(state, which)
    {:ok, Map.put(changes, :conn, put(actor(state, who), %{"name" => name}))}
  end

  defstep ~r/^(?<who>\w+) once released another number of (?<which>a new name|that name)$/,
          %{who: who, which: which},
          state do
    node = actor(state, who)
    {name, changes} = new_name(state, which)
    other = other_number(node.number)
    Names.release(name, other, node.endpoint, "personal")
    {:ok, Map.put(changes, :released, other)}
  end

  defstep ~r/^another key holds the number of (?<who>\w+) with (?<which>a new name|that name)$/,
          %{who: who, which: which},
          state do
    {name, changes} = new_name(state, which)
    number = actor(state, who).number
    Repo.insert!(%Account{endpoint: new_node().endpoint, name: name, number: number})
    {:ok, changes}
  end

  defstep ~r/^(?<who>\w+) changes her type to (?<kind>\w+)$/, %{who: who, kind: kind}, state do
    {:ok, %{conn: put(actor(state, who), %{"kind" => kind})}}
  end

  defstep ~r/^(?<who>\w+) makes herself searchable$/, %{who: who}, state do
    {:ok, %{conn: put(actor(state, who), %{"searchable" => true})}}
  end

  defstep ~r/^(?<who>\w+) renames herself (?<times>\d+) times$/,
          %{who: who, times: times},
          state do
    node = actor(state, who)

    statuses =
      for _ <- 1..String.to_integer(times), do: put(node, %{"name" => unique_name()}).status

    {:ok, %{statuses: statuses}}
  end

  defstep ~r/^the first 5 renames pass and the sixth gets 429$/, _, %{statuses: statuses} do
    assert statuses == [200, 200, 200, 200, 200, 429]
  end

  defstep ~r/^(?<who>\w+) can still claim her current name again$/, %{who: who}, state do
    node = actor(state, who)
    current = json(request(node, "GET", "/v1/account"))["name"]
    assert put(node, %{"name" => current}).status == 200
  end

  # Deletion

  defstep ~r/^(?<who>\w+) invited (?<other>\w+)$/, %{who: who, other: other}, state do
    invite(actor(state, who), actor(state, other))
  end

  defstep ~r/^(?<who>\w+) deleted her account$/, %{who: who}, state do
    assert request(actor(state, who), "DELETE", "/v1/account").status == 204
  end

  defstep ~r/^(?<who>\w+) has no invites, and his invite version moved on$/,
          %{who: who},
          %{version: version} = state do
    assert %{"version" => later, "invites" => []} = invites(actor(state, who))
    assert later > version
  end

  defstep ~r/^the account has that name and a number from 1 to 9999$/,
          _,
          %{conn: conn, new_name: name} do
    account = json(conn)
    assert account["name"] == name
    assert account["number"] in 1..9999
  end

  defstep ~r/^the account handle is the name with the number$/, _, %{conn: conn} do
    account = json(conn)
    digits = Contract.get(["names", "numberDigits"])
    number = String.pad_leading(Integer.to_string(account["number"]), digits, "0")
    assert account["handle"] == account["name"] <> "#" <> number
  end

  defstep ~r/^the account belongs to the key of (?<who>\w+)$/,
          %{who: who},
          %{conn: conn} = state do
    assert json(conn)["endpoint"] == actor(state, who).endpoint
  end

  defstep ~r/^the account is a personal agent that is not searchable$/, _, %{conn: conn} do
    assert json(conn)["kind"] == "personal"
    assert json(conn)["searchable"] == false
  end

  defstep ~r/^the account is a (?<kind>personal|service) agent$/, %{kind: kind}, %{conn: conn} do
    assert json(conn)["kind"] == kind
  end

  defstep ~r/^the account is searchable$/, _, %{conn: conn} do
    assert json(conn)["searchable"] == true
  end

  defstep ~r/^(?<who>\w+) reads back the same account$/, %{who: who}, %{conn: conn} = state do
    assert json(request(actor(state, who), "GET", "/v1/account")) == json(conn)
  end

  defstep ~r/^(?<who>\w+) keeps her number$/, %{who: who}, %{conn: conn} = state do
    assert json(conn)["number"] == actor(state, who).number
  end

  defstep ~r/^(?<who>\w+) keeps her name and number$/, %{who: who}, %{conn: conn} = state do
    node = actor(state, who)
    account = json(conn)
    assert {account["name"], account["number"]} == {node.name, node.number}
  end

  defstep ~r/^(?<who>\w+) has the new name with her old number$/,
          %{who: who},
          %{conn: conn, new_name: name} = state do
    account = json(conn)
    assert {account["name"], account["number"]} == {name, actor(state, who).number}
  end

  defstep ~r/^(?<who>\w+) has the new name with another number$/,
          %{who: who},
          %{conn: conn, new_name: name} = state do
    account = json(conn)
    assert account["name"] == name
    assert account["number"] != actor(state, who).number
  end

  defstep ~r/^(?<who>\w+) gets the number she released$/, _, %{conn: conn, released: released} do
    assert json(conn)["number"] == released
  end

  # Released tags

  defstep ~r/^the hub does not offer the old tag of (?<owner>\w+) to (?<other>\w+)$/,
          %{owner: owner, other: other},
          state do
    owner = actor(state, owner)

    assert Names.pick_number(owner.name, actor(state, other).endpoint, [owner.number]) !=
             {:ok, owner.number}
  end

  defstep ~r/^the hub still offers the old tag of (?<owner>\w+) to (?<other>\w+)$/,
          %{owner: owner, other: other},
          state do
    owner = actor(state, owner)

    assert Names.pick_number(owner.name, actor(state, other).endpoint, [owner.number]) ==
             {:ok, owner.number}
  end

  defstep ~r/^(?<other>\w+) gets another number when he claims the old name of (?<owner>\w+)$/,
          %{other: other, owner: owner},
          state do
    owner = actor(state, owner)
    assert json(put(actor(state, other), %{"name" => owner.name}))["number"] != owner.number
  end
end
