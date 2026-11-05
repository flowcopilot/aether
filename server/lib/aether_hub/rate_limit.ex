defmodule AetherHub.RateLimit do
  @moduledoc """
  Fixed windows per bucket and key, kept in ETS. Each bucket has a limit and
  a window length in seconds; a limit of 0 turns the bucket off. The process
  owns the table and deletes rows whose window ended once a minute.

  | Bucket | Key | Default |
  | --- | --- | --- |
  | `request` | endpoint | 120 a minute |
  | `search` | endpoint | 30 a minute |
  | `search_ip` | client IP | 120 a minute |
  | `rename` | endpoint | 5 a day |
  | `new_account_ip` | client IP | 20 an hour |
  | `access_request` | endpoint | 10 a day |
  | `directory_ip` | client IP | 60 a minute |
  """
  use GenServer

  @table :aether_hub_rate_limit
  @defaults %{
    request: {120, 60},
    search: {30, 60},
    search_ip: {120, 60},
    rename: {5, 86_400},
    new_account_ip: {20, 3_600},
    access_request: {10, 86_400},
    directory_ip: {60, 60}
  }

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  @doc "Counts one hit of `key` in `bucket`. Returns `:ok` or `:rate_limited`."
  def hit(bucket, key) do
    case limit(bucket) do
      {0, _seconds} ->
        :ok

      {limit, seconds} ->
        window = div(System.system_time(:second), seconds)
        row = {bucket, key, window}
        count = :ets.update_counter(@table, row, {2, 1}, {row, 0, (window + 1) * seconds})
        if count > limit, do: :rate_limited, else: :ok
    end
  end

  @doc "The `{limit, seconds}` of `bucket`, with the configured overrides."
  def limit(bucket) do
    overrides = Application.get_env(:aether_hub, :rate_limits, %{})
    Map.get(overrides, bucket) || Map.fetch!(@defaults, bucket)
  end

  @impl true
  def init(nil) do
    :ets.new(@table, [:named_table, :public, :set, write_concurrency: true])
    schedule()
    {:ok, nil}
  end

  @impl true
  def handle_info(:sweep, state) do
    now = System.system_time(:second)
    :ets.select_delete(@table, [{{:_, :_, :"$1"}, [{:<, :"$1", now}], [true]}])
    schedule()
    {:noreply, state}
  end

  defp schedule, do: Process.send_after(self(), :sweep, 60_000)
end
