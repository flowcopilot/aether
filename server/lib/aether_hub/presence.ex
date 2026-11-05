defmodule AetherHub.Presence do
  @moduledoc """
  Whether a node is online. Kept in memory only, so it starts empty at boot.

  A node is online when it has an open long poll now, or when its last signed
  request was at most 45 s ago. Presence never changes a version, so it
  never wakes a long poll. Entries older than 7 days are deleted every hour.
  """
  use GenServer

  @table :aether_hub_presence
  @window 45
  @keep 7 * 86_400
  @sweep_every :timer.hours(1)

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  @doc "Records a signed request from `endpoint` at Unix time `now`."
  def seen(endpoint, now \\ System.system_time(:second)) do
    :ets.insert(@table, {endpoint, now})
    :ok
  end

  @doc "The Unix time of the last signed request, or nil."
  def last_seen(endpoint) do
    case :ets.lookup(@table, endpoint) do
      [{^endpoint, time}] -> time
      [] -> nil
    end
  end

  @doc "Whether `endpoint` counts as online at Unix time `now`."
  def online?(endpoint, now \\ System.system_time(:second)) do
    waiting?(endpoint) or recent?(last_seen(endpoint), now)
  end

  defp waiting?(endpoint), do: Registry.lookup(AetherHub.Waiters, endpoint) != []

  defp recent?(nil, _now), do: false
  defp recent?(time, now), do: now - time <= @window

  @doc "Deletes the entries of requests older than 7 days at Unix time `now`."
  def sweep(now \\ System.system_time(:second)) do
    cutoff = now - @keep
    :ets.select_delete(@table, [{{:_, :"$1"}, [{:<, :"$1", cutoff}], [true]}])
    :ok
  end

  @impl true
  def init(nil) do
    :ets.new(@table, [:named_table, :public, :set, write_concurrency: true])
    Process.send_after(self(), :sweep, @sweep_every)
    {:ok, nil}
  end

  @impl true
  def handle_info(:sweep, state) do
    sweep()
    Process.send_after(self(), :sweep, @sweep_every)
    {:noreply, state}
  end
end
