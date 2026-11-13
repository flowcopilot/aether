defmodule AetherHub.AccountCacheTest do
  @moduledoc """
  A signed request reads its account from the cache, so every write to an
  account must end the copy that the cache holds: a rename, a change of the
  search setting, a deletion and a claim. A request after the write, from
  the same key, never sees the account as it was.
  """
  use ExUnit.Case, async: true

  import Ecto.Query
  import AetherHub.TestClient

  alias AetherHub.{Account, AccountCache, Accounts, Clock, Repo}

  defp account(node), do: json(request(node, "GET", "/v1/account"))

  defp cached?(node), do: :ets.lookup(:aether_hub_accounts, node.endpoint) != []

  defp queries(fun) do
    test = self()
    id = {__MODULE__, make_ref()}

    :telemetry.attach(
      id,
      [:aether_hub, :repo, :query],
      fn _event, _measure, _meta, _config -> if self() == test, do: send(test, {:query, id}) end,
      nil
    )

    try do
      fun.()
      count_queries(id, 0)
    after
      :telemetry.detach(id)
    end
  end

  defp count_queries(id, n) do
    receive do
      {:query, ^id} -> count_queries(id, n + 1)
    after
      0 -> n
    end
  end

  test "a signed request loads its account once, and not again" do
    node = claimed_node()
    assert account(node)["handle"] == node.handle
    assert cached?(node)

    assert queries(fn -> assert account(node)["handle"] == node.handle end) == 0
  end

  test "the cache holds no version, so nobody reads a counter that it cannot keep" do
    node = claimed_node()
    account(node)

    assert [{_, %Account{version: nil}, _loaded_at}] =
             :ets.lookup(:aether_hub_accounts, node.endpoint)

    assert Repo.get!(Account, node.endpoint).version > 0
  end

  test "a key without an account is not kept" do
    node = new_node()

    assert request(node, "GET", "/v1/account").status == 404
    refute cached?(node)
  end

  test "a first claim shows at once" do
    node = new_node()
    assert request(node, "GET", "/v1/account").status == 404

    name = unique_name()
    assert request(node, "PUT", "/v1/account", %{"name" => name}).status == 200
    assert account(node)["name"] == name
  end

  test "a rename shows at once" do
    node = claimed_node()
    assert account(node)["name"] == node.name
    assert cached?(node)

    name = unique_name()
    assert request(node, "PUT", "/v1/account", %{"name" => name}).status == 200
    refute cached?(node)
    assert account(node)["name"] == name
  end

  test "a change of the search setting shows at once" do
    node = claimed_node()
    assert account(node)["searchable"] == false

    assert request(node, "PUT", "/v1/account", %{"searchable" => true}).status == 200
    assert account(node)["searchable"] == true

    assert request(node, "PUT", "/v1/account", %{"searchable" => false}).status == 200
    assert account(node)["searchable"] == false
  end

  test "a refused change keeps the account as it was" do
    node = claimed_node()
    assert account(node)["kind"] == "personal"

    assert request(node, "PUT", "/v1/account", %{"kind" => "service"}).status == 400
    assert account(node)["kind"] == "personal"
    assert account(node)["name"] == node.name
  end

  test "a deleted account cannot act" do
    node = claimed_node()
    assert account(node)["handle"] == node.handle
    assert cached?(node)

    assert request(node, "DELETE", "/v1/account").status == 204
    refute cached?(node)

    assert request(node, "GET", "/v1/account").status == 404
    assert request(node, "GET", "/v1/contacts").status == 403
    assert request(node, "GET", "/v1/blocks").status == 403

    assert request(node, "POST", "/v1/invites", %{"to" => new_node().endpoint, "days" => 1}).status ==
             403
  end

  test "a key that deleted its account claims a new name, and it shows" do
    node = claimed_node()
    account(node)
    assert request(node, "DELETE", "/v1/account").status == 204
    assert request(node, "GET", "/v1/account").status == 404

    name = unique_name()
    assert request(node, "PUT", "/v1/account", %{"name" => name}).status == 200
    assert account(node)["name"] == name
  end

  test "a service agent that renames loses its listing, and the cache has the new name" do
    node = claimed_node("service")
    put = put_listing(node, listing(node))
    assert put.status == 200
    assert account(node)["handle"] == node.handle

    name = unique_name()
    assert request(node, "PUT", "/v1/account", %{"name" => name}).status == 200
    assert account(node)["name"] == name
    assert request(node, "GET", "/v1/listing").status == 404

    # A listing needs the new name: the old one is refused.
    assert put_listing(node, listing(node)).status == 400
  end

  test "a reader never keeps an account that a write ended while it loaded" do
    node = claimed_node()
    endpoint = node.endpoint
    me = self()

    readers =
      for _ <- 1..4 do
        spawn_link(fn ->
          loop = fn loop ->
            receive do
              :stop -> send(me, {:done, self()})
            after
              0 ->
                AccountCache.fetch(endpoint)
                loop.(loop)
            end
          end

          loop.(loop)
        end)
      end

    for i <- 1..40 do
      name = "w#{i}x#{unique_name()}"
      account = Accounts.get(endpoint)
      assert {:ok, _} = Accounts.put(endpoint, account, %{"name" => name})

      # The write returned: the cache never answers with the old name, and
      # the table never holds a copy of it, not even for a moment.
      assert AccountCache.fetch(endpoint).name == name

      for _ <- 1..50 do
        case :ets.lookup(:aether_hub_accounts, endpoint) do
          [{_, %Account{name: held}, _loaded_at}] -> assert held == name
          _ -> :ok
        end
      end
    end

    for reader <- readers, do: send(reader, :stop)
    for reader <- readers, do: assert_receive({:done, ^reader}, 1_000)

    assert AccountCache.fetch(endpoint) == %{Repo.get!(Account, endpoint) | version: nil}
  end

  describe "the time to live" do
    # A write that the hub does not see: no `invalidate/1`.
    defp write_outside(node, changes) do
      from(a in Account, where: a.endpoint == ^node.endpoint)
      |> Repo.update_all(set: changes)
    end

    setup do
      on_exit(fn -> Clock.set(nil) end)
    end

    test "a copy ends after 60 seconds, and a write from outside shows then" do
      node = claimed_node()
      start = Clock.now()
      Clock.set(start)

      assert AccountCache.fetch(node.endpoint).searchable == false
      write_outside(node, searchable: true)

      Clock.set(start + 59)
      assert AccountCache.fetch(node.endpoint).searchable == false

      Clock.set(start + 60)
      assert AccountCache.fetch(node.endpoint).searchable == true

      # The new copy lives its own 60 seconds, without a read of the database.
      Clock.set(start + 61)
      assert queries(fn -> assert AccountCache.fetch(node.endpoint).searchable == true end) == 0
    end

    test "a copy from a clock that is ahead is not trusted by a clock that is behind" do
      node = claimed_node()
      start = Clock.now()

      Clock.set(start + 1_000)
      assert AccountCache.fetch(node.endpoint).searchable == false
      write_outside(node, searchable: true)

      Clock.set(start)
      assert AccountCache.fetch(node.endpoint).searchable == true
    end
  end

  describe "the mark of a reader" do
    test "a reader that finds the mark of another one reads, and stores nothing" do
      node = claimed_node()
      mark = {make_ref(), System.monotonic_time(:second)}
      :ets.insert(:aether_hub_accounts, {node.endpoint, :loading, mark})

      assert AccountCache.fetch(node.endpoint).name == node.name
      assert :ets.lookup(:aether_hub_accounts, node.endpoint) == [{node.endpoint, :loading, mark}]

      AccountCache.invalidate(node.endpoint)
    end

    test "a mark that a killed reader left is taken over" do
      node = claimed_node()
      mark = {make_ref(), System.monotonic_time(:second) - 11}
      :ets.insert(:aether_hub_accounts, {node.endpoint, :loading, mark})

      assert AccountCache.fetch(node.endpoint).name == node.name
      assert cached?(node)
      assert [{_, %Account{}, _}] = :ets.lookup(:aether_hub_accounts, node.endpoint)
    end
  end
end
