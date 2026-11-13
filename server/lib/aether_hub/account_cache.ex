defmodule AetherHub.AccountCache do
  @moduledoc """
  The accounts that signed requests load, kept in memory by endpoint, so a
  signed request does not read the database for its own account.

  The cache is right only while every write to the `accounts` table ends
  with `invalidate/1` for that endpoint. The writes are in
  `AetherHub.Accounts` (a claim, a change, a rename and a deletion), and
  each one invalidates inside its transaction and again after it. The
  counters in `AetherHub.Versions` change `version` only, and the cache does
  not keep a version: the copy of an account that it returns has
  `version: nil`, so a reader of the counter fails loudly and reads
  `AetherHub.Versions.current/1` instead. A new write to `accounts` anywhere
  else must call `invalidate/1`.

  A reader that misses loads the row and stores it, unless a write ended in
  between. Every `invalidate/1` also counts up a number: the reader notes it
  before it loads the row, and it takes its copy back out when the number
  changed. So a copy that was loaded before a write never stays.

  An endpoint without an account is never stored, so unknown keys cannot
  fill the table. The process owns the table and does nothing else.
  """
  use GenServer

  alias AetherHub.{Account, Repo}

  @table :aether_hub_accounts
  # The numbers that count the writes: one for each group of endpoints, so
  # a write to one account rarely drops the copy that another one loads.
  @slots 1024

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  @doc "The account of `endpoint`, or `nil`, as the database has it."
  def fetch(endpoint) do
    case :ets.lookup(@table, endpoint) do
      [{^endpoint, account}] -> account
      [] -> load(endpoint)
    end
  end

  @doc """
  Forgets the account of `endpoint`. Call it for every write to its row,
  inside the transaction that makes the write and again after it.
  """
  def invalidate(endpoint) do
    :ets.update_counter(@table, slot(endpoint), 1)
    :ets.delete(@table, endpoint)
    :ok
  end

  defp load(endpoint) do
    epoch = epoch(endpoint)

    case Repo.get(Account, endpoint) do
      nil ->
        nil

      account ->
        account = %{account | version: nil}
        :ets.insert(@table, {endpoint, account})
        # A write that ended since the read above has counted the number
        # up: the copy may be older than the row.
        if epoch(endpoint) != epoch, do: :ets.delete_object(@table, {endpoint, account})
        account
    end
  end

  defp epoch(endpoint), do: :ets.lookup_element(@table, slot(endpoint), 2)

  defp slot(endpoint), do: {:epoch, :erlang.phash2(endpoint, @slots)}

  @impl true
  def init(nil) do
    :ets.new(@table, [:named_table, :public, :set, read_concurrency: true])
    :ets.insert(@table, for(slot <- 0..(@slots - 1), do: {{:epoch, slot}, 0}))
    {:ok, nil}
  end
end
