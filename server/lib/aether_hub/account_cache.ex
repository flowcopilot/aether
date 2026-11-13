defmodule AetherHub.AccountCache do
  @moduledoc """
  The accounts that signed requests load, kept in memory by endpoint, so a
  signed request does not read the database for its own account.

  ## What keeps a copy right

  The cache is right only while every write to the `accounts` table ends
  with `invalidate/1` for that endpoint. The writes are in
  `AetherHub.Accounts` (a claim, a change, a rename and a deletion), and
  each one invalidates inside its transaction and again after it. The
  counters in `AetherHub.Versions` change `version` only, and the cache does
  not keep a version: the copy of an account that it returns has
  `version: nil`, so a reader of the counter fails loudly and reads
  `AetherHub.Versions.current/1` instead. A new write to `accounts` anywhere
  else must call `invalidate/1`.

  A copy is also dropped after 60 seconds (`AetherHub.Clock` time), so a
  write that does not go through this VM, such as a `sqlite3` shell or
  another hub on the same file, shows after one minute at most. The cache
  cannot see such a write, and nothing else ends the copy.

  ## How a reader stores a copy

  A reader that misses does not insert its copy blindly. It first claims the
  endpoint with `:ets.insert_new/2`, which puts a `:loading` mark in the
  row. Then it reads the database, and `:ets.select_replace/2` turns its own
  mark into the copy, in one atomic step on that one row. `invalidate/1`
  deletes the row, mark or copy, so:

    * a write that ends after the claim deletes the mark, the replace finds
      no mark and stores nothing (the reader still answers with what it
      read, which is not older than its request);
    * a write that ends after the copy is stored deletes the copy.

  A copy that a finished write overtook is never in the table, not even for
  a moment: there is no insert that a delete has to undo. A reader that
  finds a mark of another reader does not wait: it reads the database and
  stores nothing. The reader removes its mark when it fails, and a mark
  older than 10 seconds, left by a process that was killed, is taken over.

  An endpoint without an account is never stored, so unknown keys cannot
  fill the table. The process owns the table and does nothing else.
  """
  use GenServer

  alias AetherHub.{Account, Clock, Repo}

  @table :aether_hub_accounts
  # How long a copy stays, in seconds.
  @ttl 60
  # How long a mark of a reader that never came back stays, in seconds.
  @claim_ttl 10

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  @doc "The account of `endpoint`, or `nil`, as the database has it."
  def fetch(endpoint) do
    case :ets.lookup(@table, endpoint) do
      [{^endpoint, %Account{} = account, loaded_at}] = row ->
        if fresh?(loaded_at), do: account, else: reload(endpoint, row)

      [{^endpoint, :loading, {_ref, started}}] = row ->
        if System.monotonic_time(:second) - started > @claim_ttl,
          do: reload(endpoint, row),
          else: read(endpoint)

      [] ->
        claim(endpoint)
    end
  end

  @doc """
  Forgets the account of `endpoint`. Call it for every write to its row,
  inside the transaction that makes the write and again after it.
  """
  def invalidate(endpoint) do
    :ets.delete(@table, endpoint)
    :ok
  end

  # A copy of a clock that is behind its own load time (a test moved it) is
  # not trusted.
  defp fresh?(loaded_at) do
    now = Clock.now()
    loaded_at <= now and now - loaded_at < @ttl
  end

  # An old copy or an abandoned mark goes, and the reader claims the row. If
  # another reader claimed it first, this one only reads.
  defp reload(endpoint, [row]) do
    :ets.delete_object(@table, row)
    claim(endpoint)
  end

  defp claim(endpoint) do
    mark = {make_ref(), System.monotonic_time(:second)}

    if :ets.insert_new(@table, {endpoint, :loading, mark}) do
      load(endpoint, mark)
    else
      read(endpoint)
    end
  end

  defp load(endpoint, mark) do
    # The time before the read: the copy is not younger than this.
    loaded_at = Clock.now()

    case read(endpoint) do
      nil ->
        nil

      account ->
        spec = [{{endpoint, :loading, mark}, [], [{{endpoint, {:const, account}, loaded_at}}]}]
        :ets.select_replace(@table, spec)
        account
    end
  after
    # Gone already when the copy replaced it, or when a write took it.
    :ets.delete_object(@table, {endpoint, :loading, mark})
  end

  defp read(endpoint) do
    case Repo.get(Account, endpoint) do
      nil -> nil
      account -> %{account | version: nil}
    end
  end

  @impl true
  def init(nil) do
    :ets.new(@table, [:named_table, :public, :set, read_concurrency: true])
    {:ok, nil}
  end
end
