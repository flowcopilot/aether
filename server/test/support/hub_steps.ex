defmodule AetherHub.HubSteps do
  @moduledoc """
  The steps and helpers that every hub feature shares. A scenario keeps its
  nodes in the state under `:actors`, by the name that the feature gives
  them (such as `alice` or `law`), and the last answer under `:conn`.
  """
  use AetherHub.Feature.Steps

  import ExUnit.Assertions
  import AetherHub.TestClient

  alias AetherHub.{Clock, Contract, Sweeper}

  @bundle Contract.get(["limits", "invitePrefix"]) <> "signed-line"

  ## Actors

  @doc "The node that the scenario calls `name`."
  def actor(state, name) do
    case state |> Map.get(:actors, %{}) |> Map.fetch(name) do
      {:ok, node} -> node
      :error -> flunk("the scenario has no node called #{inspect(name)}")
    end
  end

  @doc "The state change that remembers `node` as `name`."
  def put_actor(state, name, node),
    do: %{actors: state |> Map.get(:actors, %{}) |> Map.put(name, node)}

  @doc "The names in a text such as `alice, bob and carol`."
  def split_names(text), do: String.split(text, ~r/, | and /, trim: true)

  @kinds %{
    "person" => :personal,
    "people" => :personal,
    "service agent" => :service,
    "service agents" => :service,
    "unlisted service agent" => :service,
    "listed service agent" => :listed,
    "listed service agents" => :listed,
    "node without a name" => :new
  }

  defstep ~r/^(?<names>\w+(?:(?:, | and )\w+)*) (?:is an?|are) (?<kind>person|people|(?:un)?(?:listed )?service agents?|node without a name)$/,
          %{names: names, kind: kind},
          state do
    actors =
      for name <- split_names(names), into: Map.get(state, :actors, %{}) do
        {name, new_actor(Map.fetch!(@kinds, kind))}
      end

    {:ok, %{actors: actors}}
  end

  defp new_actor(:personal), do: claimed_node()
  defp new_actor(:service), do: claimed_node("service")
  defp new_actor(:listed), do: listed_service()
  defp new_actor(:new), do: new_node()

  defstep ~r/^(?<who>\w+) is a new node$/, %{who: who}, state do
    {:ok, put_actor(state, who, new_node())}
  end

  defstep ~r/^(?<who>\w+) has a name$/, %{who: who}, state do
    {:ok, put_actor(state, who, claimed_node())}
  end

  defstep ~r/^(?<who>\w+) has a service name$/, %{who: who}, state do
    {:ok, put_actor(state, who, claimed_node("service"))}
  end

  defstep ~r/^(?<who>\w+) takes a new name$/, %{who: who}, state do
    node = actor(state, who)
    conn = request(node, "PUT", "/v1/account", %{"name" => unique_name()})
    node = if conn.status == 200, do: with_account(node, json(conn)), else: node
    {:ok, Map.put(put_actor(state, who, node), :conn, conn)}
  end

  defstep ~r/^(?<who>\w+) deletes (?:her|his|its) account$/, %{who: who}, state do
    {:ok, %{conn: request(actor(state, who), "DELETE", "/v1/account")}}
  end

  @doc "A number next to `number` in 1..9999."
  def other_number(9999), do: 1
  def other_number(number), do: number + 1

  @doc "A service agent with a published listing."
  def listed_service(opts \\ []) do
    service = claimed_node("service")
    %{status: 200} = put_listing(service, listing(service, opts))
    service
  end

  ## Answers

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

  defstep ~r/^the hub answers 200 each time$/, _, %{statuses: statuses} do
    assert Enum.uniq(statuses) == [200]
  end

  ## Invites and contacts

  @doc "Sends an invite from `from` to the full name of `to`."
  def invite(from, to),
    do: request(from, "POST", "/v1/invites", %{"to" => to.handle, "days" => 7})

  defstep ~r/^(?<who>\w+) invites (?<other>\w+)$/, %{who: who, other: other}, state do
    conn = invite(actor(state, who), actor(state, other))
    answers = state |> Map.get(:answers, %{}) |> Map.put(who, conn)

    {:ok,
     %{
       conn: conn,
       answers: answers,
       invite_id: if(conn.status == 201, do: json(conn)["id"])
     }}
  end

  defstep ~r/^(?<who>\w+) has no invites$/, %{who: who}, state do
    assert invites(actor(state, who))["invites"] == []
  end

  @doc "The invite list of `node`, decoded."
  def invites(node), do: json(request(node, "GET", "/v1/invites"))

  @doc "GET `path` with an optional query map or string."
  def http_get(node, path, query \\ nil), do: AetherHub.HubRequests.get(node, path, query)

  @doc "The contact list of `node`. It asserts that the hub answers 200."
  def contacts(node) do
    conn = request(node, "GET", "/v1/contacts")
    assert conn.status == 200
    json(conn)["contacts"]
  end

  @doc "`from` invites `to`, `to` accepts and `from` delivers the line. Returns the id."
  def connect(from, to) do
    %{"id" => id} = json(invite(from, to))
    assert request(to, "POST", "/v1/invites/#{id}/accept").status == 200

    assert request(from, "POST", "/v1/invites/#{id}/bundle", %{"bundle" => @bundle}).status ==
             200

    id
  end

  @doc "`person` asks `service` for access, and the service approves and delivers."
  def connect_by_request(person, service) do
    conn = request(person, "POST", "/v1/requests", %{"to" => service.handle, "days" => 7})
    assert conn.status == 201
    path = "/v1/invites/#{json(conn)["id"]}"
    assert request(service, "POST", path <> "/approve").status == 200
    assert request(service, "POST", path <> "/bundle", %{"bundle" => @bundle}).status == 200
  end

  defstep ~r/^(?<one>\S+) and (?<other>\S+) are contacts$/, %{one: one, other: other}, state do
    {:ok, %{invite_id: connect(actor(state, one), actor(state, other))}}
  end

  defstep ~r/^(?<who>\w+) has no contacts$/, %{who: who}, state do
    assert contacts(actor(state, who)) == []
  end

  defstep ~r/^(?<who>\w+) has read (?:his|her) invites$/, %{who: who}, state do
    {:ok, %{version: invites(actor(state, who))["version"]}}
  end

  defstep ~r/^the invite version of (?<who>\w+) did not move$/,
          %{who: who},
          %{version: version} = state do
    assert invites(actor(state, who))["version"] == version
  end

  ## Blocks

  @doc "`owner` blocks the endpoint of `blocked`."
  def block(owner, blocked), do: request(owner, "PUT", "/v1/blocks/#{blocked.endpoint}")

  defstep ~r/^(?<who>\w+) blocks (?<other>\w+)$/, %{who: who, other: other}, state do
    node = actor(state, who)
    other = if other == "herself", do: node, else: actor(state, other)
    {:ok, %{conn: block(node, other)}}
  end

  defstep ~r/^(?<who>\w+) blocked (?<other>\w+)$/, %{who: who, other: other}, state do
    assert block(actor(state, who), actor(state, other)).status == 204
  end

  ## Owner links

  @doc "An owner link that `personal` claims and `service` confirms."
  def link_owner(personal, service) do
    %{status: 201} = conn = claim_owner(personal, service.handle)
    link = Map.put(json(conn), "personalEndpoint", personal.endpoint)
    %{status: 200} = confirm_owner(service, link)
  end

  ## Time

  @units %{"second" => 1, "minute" => 60, "hour" => 3_600, "day" => 86_400}

  defstep ~r/^it is (?<n>\d+) (?<unit>second|minute|hour|day)s? (?<way>ago|later)$/,
          %{n: n, unit: unit, way: way},
          _state do
    seconds = String.to_integer(n) * @units[unit]
    Clock.set(System.system_time(:second) + if(way == "ago", do: -seconds, else: seconds))
  end

  defstep ~r/^it is now$/, _, _state do
    Clock.set(nil)
  end

  defstep ~r/^it is the start of an? (?<unit>minute|hour|day)$/, %{unit: unit}, _state do
    seconds = @units[unit]
    Clock.set(div(System.system_time(:second), seconds) * seconds)
  end

  @doc "Runs `fun` at the Unix time `time` in this process, then returns to the time before."
  def at(time, fun) do
    previous = Clock.set(time)

    try do
      fun.()
    after
      Clock.set(previous)
    end
  end

  @doc "`node` sends a signed request at the Unix time `time`, so the hub last saw it then."
  def seen_at(node, time), do: at(time, fn -> request(node, "GET", "/v1/account") end)

  defstep ~r/^(?<who>\w+) was last (?:seen|online) long ago$/, %{who: who}, state do
    long_ago = System.system_time(:second) - 3_600
    seen_at(actor(state, who), long_ago)
    {:ok, %{seen_at: long_ago}}
  end

  defstep ~r/^the hub (?:sweeps expired \w+|cleans up presence)$/, _, _state do
    Sweeper.sweep()
  end
end
