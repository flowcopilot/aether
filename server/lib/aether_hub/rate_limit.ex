defmodule AetherHub.RateLimit do
  @moduledoc """
  Fixed windows per bucket and key, kept in ETS. Each bucket has a limit and
  a window length in seconds; a limit of 0 turns the bucket off. The
  `:rate_limits` setting overrides the defaults. Only `AetherHub.Router`
  counts requests. The process owns the table; `AetherHub.Sweeper` removes
  ended windows.
  """
  use GenServer

  alias AetherHub.Clock

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

  @doc "Counts one hit of `key` in `bucket`. `:rate_limited` when it is over the limit."
  def hit(bucket, key) do
    case limit(bucket) do
      {0, _seconds} ->
        :ok

      {limit, seconds} ->
        if update(bucket, key, seconds, 1) > limit, do: :rate_limited, else: :ok
    end
  end

  @doc "Whether one more hit of `key` in `bucket` is within the limit. Counts nothing."
  def check(bucket, key) do
    case limit(bucket) do
      {0, _seconds} ->
        :ok

      {limit, seconds} ->
        if update(bucket, key, seconds, 0) >= limit, do: :rate_limited, else: :ok
    end
  end

  @doc "The `{limit, seconds}` of `bucket`."
  def limit(bucket) do
    overrides = Application.get_env(:aether_hub, :rate_limits, %{})
    Map.get(overrides, bucket) || Map.fetch!(@defaults, bucket)
  end

  defp update(bucket, key, seconds, increment) do
    window = div(Clock.now(), seconds)
    row = {bucket, key, window}
    :ets.update_counter(@table, row, {2, increment}, {row, 0, (window + 1) * seconds})
  end

  def sweep do
    :ets.select_delete(@table, [{{:_, :_, :"$1"}, [{:<, :"$1", Clock.now()}], [true]}])
    :ok
  end

  @impl true
  def init(nil) do
    :ets.new(@table, [:named_table, :public, :set, write_concurrency: true])
    {:ok, nil}
  end
end
