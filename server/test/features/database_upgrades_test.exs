defmodule AetherHub.Features.DatabaseUpgradesTest do
  # The test application has every migration at boot, so these scenarios run
  # the migrations again on a second database.
  use AetherHub.Feature, file: "database-upgrades.feature", async: false

  alias AetherHub.Repo

  @before_names 20_261_015_120_000
  @names 20_261_016_000_000

  defstep ~r/^a hub database from before accounts had numbers$/, _, _ do
    path =
      Path.join(
        System.tmp_dir!(),
        "aether_hub_migration_#{System.unique_integer([:positive])}.db"
      )

    # The migrator loads the migration modules a second time.
    ignore = Code.get_compiler_option(:ignore_module_conflict)
    Code.put_compiler_option(:ignore_module_conflict, true)
    on_exit(fn -> Code.put_compiler_option(:ignore_module_conflict, ignore) end)

    # The repo is linked to the test process and stops with it.
    {:ok, pid} = Repo.start_link(name: nil, database: path, pool_size: 1)
    previous = Repo.put_dynamic_repo(pid)

    on_exit(fn ->
      for file <- [path, path <> "-wal", path <> "-shm"], do: File.rm(file)
    end)

    Ecto.Migrator.run(Repo, :up, to: @before_names, dynamic_repo: pid, log: false)
    {:ok, %{pid: pid, previous: previous}}
  end

  defstep ~r/^these accounts:$/, %{table: rows}, _ do
    for %{"key" => key, "name" => name, "version" => version} <- rows do
      Repo.query!("INSERT INTO accounts(endpoint, handle, version) VALUES (?1, ?2, ?3)", [
        key,
        name,
        String.to_integer(version)
      ])
    end

    {:ok, %{accounts: rows}}
  end

  defstep ~r/^a pending invite from (?<from>\w+) to (?<to>\w+)$/,
          %{from: from, to: to},
          %{accounts: accounts} do
    key = fn name -> Enum.find(accounts, &(&1["name"] == name))["key"] end

    Repo.query!(
      "INSERT INTO invites(id, sender_endpoint, receiver_endpoint, days, status, created_at) VALUES ('i1', ?1, ?2, 7, 'pending', ?3)",
      [key.(from), key.(to), System.system_time(:second)]
    )
  end

  defstep ~r/^the hub upgrades its database to the names version$/, _, state do
    %{pid: pid, previous: previous} = state
    Ecto.Migrator.run(Repo, :up, to: @names, dynamic_repo: pid, log: false)

    %{rows: rows} =
      Repo.query!("SELECT endpoint, name, number, kind, version FROM accounts ORDER BY endpoint")

    %{rows: [[tags]]} =
      Repo.query!("SELECT COUNT(DISTINCT name || '#' || number) FROM accounts")

    %{rows: invites} = Repo.query!("SELECT status FROM invites WHERE id = 'i1'")
    Repo.put_dynamic_repo(previous)
    {:ok, %{rows: rows, tags: tags, invites: invites}}
  end

  defstep ~r/^each account keeps its key and name and is a personal agent$/,
          _,
          %{rows: rows, accounts: accounts} do
    assert Enum.map(rows, fn [key, name, _, kind, _] -> [key, name, kind] end) ==
             Enum.map(accounts, &[&1["key"], &1["name"], "personal"])
  end

  defstep ~r/^each account has a number from 1 to 9999$/, _, %{rows: rows} do
    for [_, _, number, _, _] <- rows, do: assert(number in 1..9999)
  end

  defstep ~r/^the version of each account moved up by one$/,
          _,
          %{rows: rows, accounts: accounts} do
    assert Enum.map(rows, &List.last/1) ==
             Enum.map(accounts, &(String.to_integer(&1["version"]) + 1))
  end

  defstep ~r/^no two accounts have the same tag$/, _, %{rows: rows, tags: tags} do
    assert tags == length(rows)
  end

  defstep ~r/^the invite is still pending$/, _, %{invites: invites} do
    assert invites == [["pending"]]
  end
end
