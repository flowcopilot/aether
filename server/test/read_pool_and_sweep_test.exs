defmodule AetherHub.ReadPoolAndSweepTest do
  @moduledoc """
  The read pool is read-only and sees each commit of the writer, and the
  invite sweep deletes a backlog of any size in bounded transactions.

  The module is not async: a sweep removes the expired rows of every test.
  """
  use ExUnit.Case, async: false

  import Ecto.Query

  alias AetherHub.{Account, Clock, Invite, Invites, Protocol, ReadRepo, Repo, Versions}

  @ttl Protocol.limit(:invite_seconds)
  # SQLite's limit on the variables of one statement.
  @variable_limit 32_766

  setup do
    on_exit(fn ->
      Repo.delete_all(from i in Invite, where: like(i.sender_endpoint, "sweeptest-%"))
      Repo.delete_all(from a in Account, where: like(a.endpoint, "sweeptest-%"))
    end)
  end

  defp accounts(count) do
    rows =
      for i <- 1..count do
        %{
          endpoint: "sweeptest-#{i}",
          name: "sweeptest#{i}",
          number: 1,
          kind: "personal",
          searchable: false,
          created_at: 0,
          version: 0
        }
      end

    rows |> Enum.chunk_every(500) |> Enum.each(&Repo.insert_all(Account, &1))
    for i <- 1..count, do: "sweeptest-#{i}"
  end

  # One invite for each ordered pair of `endpoints` that `pairs?` accepts.
  defp insert_invites(endpoints, created_at, pairs? \\ fn _from, _to -> true end) do
    rows =
      for from <- endpoints, to <- endpoints, from != to, pairs?.(from, to) do
        %{
          id: Repo.new_id(),
          sender_endpoint: from,
          receiver_endpoint: to,
          days: 7,
          status: "pending",
          origin: "invite",
          created_at: created_at
        }
      end

    rows |> Enum.chunk_every(500) |> Enum.each(&Repo.insert_all(Invite, &1))
    length(rows)
  end

  defp expired_at, do: Clock.now() - @ttl - 10

  defp invite_count,
    do: Repo.aggregate(from(i in Invite, where: like(i.sender_endpoint, "sweeptest-%")), :count)

  defp version(endpoint), do: Versions.current(endpoint)

  describe "the read pool" do
    test "refuses a write" do
      # `read_only: true` leaves the write functions out of the repo.
      refute function_exported?(ReadRepo, :insert, 1)
      refute function_exported?(ReadRepo, :delete_all, 1)

      assert {:error, %Exqlite.Error{message: message}} =
               ReadRepo.query("DELETE FROM invites")

      assert message =~ "readonly"
    end

    test "sees each commit of the writer, also under load from other writes" do
      [endpoint] = accounts(1)

      for expected <- 1..100 do
        assert :ok = Versions.commit(fn -> {:ok, [endpoint]} end)
        assert version(endpoint) == expected
      end
    end

    test "a long poll wakes after the commit and reads the new version" do
      [endpoint] = accounts(1)
      account = Repo.get!(Account, endpoint)

      poll =
        Task.async(fn ->
          Invites.list(account, %{"after" => "0", "wait" => "20"})
        end)

      # Wait until the poll waits, then change the account.
      Enum.reduce_while(1..200, nil, fn _, _ ->
        if AetherHub.Notify.waiting?(endpoint),
          do: {:halt, nil},
          else: Process.sleep(10) && {:cont, nil}
      end)

      assert :ok = Versions.commit(fn -> {:ok, [endpoint]} end)
      assert {:ok, %{"version" => 1, "invites" => []}} = Task.await(poll, 5_000)
    end

    test "the snapshot reads the rows and the version in one state" do
      [a, b] = accounts(2)
      insert_invites([a, b], Clock.now())
      account = Repo.get!(Account, a)

      assert {:ok, %{"version" => 0, "invites" => [_, _]}} = Invites.list(account, %{})
    end
  end

  describe "the sweep" do
    test "deletes in chunks, keeps open rows and bumps each endpoint of the swept rows" do
      endpoints = accounts(6)
      # 30 ordered pairs: the pairs from the first three accounts are
      # expired, the others open.
      expired_from = Enum.take(endpoints, 3)

      insert_invites(endpoints, expired_at(), fn from, _to -> from in expired_from end)
      open = insert_invites(endpoints, Clock.now(), fn from, _to -> from not in expired_from end)
      assert invite_count() == 30

      before = Map.new(endpoints, &{&1, version(&1)})

      # A chunk of 4 needs several transactions for 15 rows.
      assert :ok = Invites.delete_expired(4)

      assert invite_count() == open

      # Every account is the sender or the receiver of an expired row.
      for endpoint <- endpoints, do: assert(version(endpoint) > before[endpoint])
    end

    test "a sweep of nothing changes nothing" do
      [endpoint] = accounts(1)
      assert :ok = Invites.delete_expired()
      assert version(endpoint) == 0
    end

    test "deletes more expired rows than SQLite has variables" do
      # 200 accounts make 39,800 ordered pairs.
      endpoints = accounts(200)
      total = insert_invites(endpoints, expired_at())
      assert total > @variable_limit
      assert invite_count() == total

      assert :ok = Invites.delete_expired()
      assert invite_count() == 0
      assert version(hd(endpoints)) > 0
    end

    test "a bump of more endpoints than SQLite has variables succeeds" do
      [endpoint] = accounts(1)
      others = for i <- 1..(@variable_limit + 1000), do: "sweeptest-none-#{i}"

      assert :ok = Versions.commit(fn -> {:ok, [endpoint | others] ++ [endpoint]} end)
      # One bump for an endpoint that the list names twice.
      assert version(endpoint) == 1
    end
  end
end
