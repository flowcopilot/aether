defmodule AetherHub.Presence do
  @moduledoc """
  Whether a node is online. Kept in memory only, so it starts empty at boot.

  A node is online when it has an open long poll now, or when its last signed
  request was recent. Presence never changes a version, so it never wakes a
  long poll. The process owns the table; `AetherHub.Sweeper` removes old
  entries.
  """
  use GenServer

  alias AetherHub.Clock

  @table :aether_hub_presence
  @window AetherHub.Protocol.limit(:online_seconds)
  @keep AetherHub.Protocol.limit(:presence_keep_seconds)

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  @doc "Records a signed request from `endpoint` now."
  def seen(endpoint) do
    :ets.insert(@table, {endpoint, Clock.now()})
    :ok
  end

  @doc "The Unix time of the last signed request, or nil."
  def last_seen(endpoint) do
    case :ets.lookup(@table, endpoint) do
      [{^endpoint, time}] -> time
      [] -> nil
    end
  end

  def online?(endpoint) do
    waiting?(endpoint) or recent?(last_seen(endpoint))
  end

  defp waiting?(endpoint), do: Registry.lookup(AetherHub.Waiters, endpoint) != []

  defp recent?(nil), do: false
  defp recent?(time), do: Clock.now() - time <= @window

  @doc "Forgets the requests that are older than the time the hub keeps them."
  def sweep do
    cutoff = Clock.now() - @keep
    :ets.select_delete(@table, [{{:_, :"$1"}, [{:<, :"$1", cutoff}], [true]}])
    :ok
  end

  @impl true
  def init(nil) do
    :ets.new(@table, [:named_table, :public, :set, write_concurrency: true])
    {:ok, nil}
  end
end
