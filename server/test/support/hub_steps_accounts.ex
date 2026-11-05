defmodule AetherHub.HubStepsAccounts do
  @moduledoc """
  Helpers for the step definitions of the account, request, limit, block,
  contact and people features. A scenario keeps its nodes in the state
  under `:nodes`, by the name the feature gives them (such as `alice`).
  """

  import ExUnit.Assertions
  import AetherHub.TestClient

  @bundle "aether-invite:signed-line"

  @doc "The node that the scenario calls `name`."
  def who(state, name) do
    case state |> Map.get(:nodes, %{}) |> Map.fetch(name) do
      {:ok, node} -> node
      :error -> flunk("the scenario has no node called #{inspect(name)}")
    end
  end

  @doc "The state change that remembers `node` as `name`."
  def remember(state, name, node),
    do: %{nodes: state |> Map.get(:nodes, %{}) |> Map.put(name, node)}

  @doc "Sends an invite from `from` to the handle of `to`."
  def invite(from, to),
    do: request(from, "POST", "/v1/invites", %{"to" => to.handle, "days" => 7})

  @doc "The invite list of `node`, decoded."
  def invites(node), do: json(request(node, "GET", "/v1/invites"))

  @doc "The contact list of `node`. It asserts that the hub answers 200."
  def contacts(node) do
    conn = request(node, "GET", "/v1/contacts")
    assert conn.status == 200
    json(conn)["contacts"]
  end

  @doc "`from` invites `to` and `to` accepts. Returns the invite id."
  def invite_and_accept(from, to) do
    %{"id" => id} = json(invite(from, to))
    assert request(to, "POST", "/v1/invites/#{id}/accept").status == 200
    id
  end

  @doc "`from` invites `to`, `to` accepts and `from` delivers the bundle."
  def connect(from, to) do
    id = invite_and_accept(from, to)

    assert request(from, "POST", "/v1/invites/#{id}/bundle", %{"bundle" => @bundle}).status ==
             200

    id
  end

  @doc "A number next to `number` in 1..9999."
  def other_number(9999), do: 1
  def other_number(number), do: number + 1

  @doc """
  Waits for a new fixed window when fewer than `margin` seconds are left in
  the current one, so that the window does not roll over mid-scenario.
  """
  def start_early_in_window(window, margin) do
    second = rem(System.system_time(:second), window)
    if second > window - margin, do: Process.sleep((window + 1 - second) * 1000)
  end
end
