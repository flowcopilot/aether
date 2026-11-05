defmodule AetherHub.HubStepsSocial do
  @moduledoc """
  Helpers for the step definitions of the invite, access request, directory,
  listing, agent search and owner link features. A scenario keeps its nodes
  in the state under `:actors`, by the name the feature gives them (such as
  `alice` or `law`).
  """

  import ExUnit.Assertions
  import AetherHub.TestClient

  alias AetherHub.OwnerLinks

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

  @doc """
  The state change for a step such as `alice and bob are people`: one new
  node of `kind` for each name.
  """
  def new_actors(state, names, kind) do
    actors =
      for name <- split_names(names), into: Map.get(state, :actors, %{}) do
        {name, new_actor(kind)}
      end

    %{actors: actors}
  end

  @doc "The names in a text such as `alice, bob and carol`."
  def split_names(text), do: String.split(text, ~r/, | and /, trim: true)

  defp new_actor(kind) when kind in ["person", "people"], do: claimed_node()

  defp new_actor(kind)
       when kind in ["service agent", "service agents", "unlisted service agent"],
       do: claimed_node("service")

  defp new_actor(kind) when kind in ["listed service agent", "listed service agents"],
    do: listed_service()

  defp new_actor("node without a name"), do: new_node()

  @doc "A service agent with a published listing."
  def listed_service(opts \\ []) do
    service = claimed_node("service")
    %{status: 200} = put_listing(service, listing(service, opts))
    service
  end

  @doc "An owner link that `personal` claims and `service` confirms."
  def link_owner(personal, service) do
    expires_at = System.system_time(:second) + 30 * 86_400
    claim = OwnerLinks.claim_message(personal.endpoint, service.handle, expires_at)

    %{status: 201} =
      conn =
      request(personal, "POST", "/v1/owner-links", %{
        "service" => service.handle,
        "expiresAt" => expires_at,
        "signature" => sign(personal, claim)
      })

    id = json(conn)["id"]
    confirm = OwnerLinks.confirm_message(service.endpoint, personal.endpoint, expires_at)

    %{status: 200} =
      request(service, "POST", "/v1/owner-links/#{id}/confirm", %{
        "signature" => sign(service, confirm)
      })
  end

  @doc "Asserts the status of the last answer in the state."
  def assert_status(%{conn: conn}, status), do: assert(conn.status == to_int(status))

  @doc "Asserts the status and the error body of the last answer in the state."
  def assert_error(%{conn: conn}, status, error) do
    assert conn.status == to_int(status)
    assert json(conn) == %{"error" => error}
  end

  @doc "The integer in a text."
  def to_int(text) when is_binary(text), do: String.to_integer(text)
  def to_int(number) when is_integer(number), do: number

  @doc "A random word that no other scenario uses, for a scenario's search."
  def marker, do: unique_name()

  @doc """
  Waits for a new fixed window when fewer than `margin` seconds are left in
  the current one, so that the window does not roll over mid-scenario.
  """
  def start_early_in_window(window, margin) do
    second = rem(System.system_time(:second), window)
    if second > window - margin, do: Process.sleep((window + 1 - second) * 1000)
  end
end
