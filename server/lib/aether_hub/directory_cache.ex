defmodule AetherHub.DirectoryCache do
  @moduledoc """
  Keeps the built directory for 15 seconds, and builds it once at a time.

  A read inside the 15 seconds takes the value from an ETS table and sends
  no message. The first read after that asks this process to build a new
  value. The other readers wait in its mailbox; each one finds the new value
  when its turn comes, so a crowd of readers makes one build, not one each.
  A build that fails serves the old value when there is one.

  The time to live is `config :aether_hub, :directory_cache_ms`, 15 seconds
  when it is not set. At `0` there is no cache: the caller builds the value
  itself, which is what the tests do, because they set the clock of their
  own process.

  `start_link/1` also takes `:name`, `:build` (a function with no argument),
  `:ttl` (milliseconds, or a function that returns them) and `:now` (a
  function that returns milliseconds), for a second cache with its own
  table.
  """
  use GenServer

  @default_ttl :timer.seconds(15)
  @call_timeout :timer.seconds(30)

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: Keyword.get(opts, :name, __MODULE__))
  end

  @doc "The directory, at most the time to live old."
  def get(server \\ __MODULE__) do
    [{:config, build, ttl, now}] = :ets.lookup(server, :config)

    ttl = ttl(ttl)

    if ttl <= 0 do
      build.()
    else
      case fresh(server, ttl, now) do
        nil -> server |> GenServer.call(:refresh, @call_timeout) |> unwrap(server)
        value -> value
      end
    end
  end

  @doc "Forgets the value: the next read builds a new one."
  def clear(server \\ __MODULE__), do: GenServer.call(server, :clear)

  defp fresh(server, ttl, now) do
    with [{:value, built_at, value}] <- :ets.lookup(server, :value),
         true <- now.() - built_at < ttl do
      value
    else
      _ -> nil
    end
  end

  defp unwrap({:ok, value}, _server), do: value

  defp unwrap({:error, kind, reason, stacktrace}, server) do
    # The old value is better than an error page.
    case :ets.lookup(server, :value) do
      [{:value, _built_at, value}] -> value
      [] -> :erlang.raise(kind, reason, stacktrace)
    end
  end

  defp ttl(fun) when is_function(fun, 0), do: fun.()
  defp ttl(ms) when is_integer(ms), do: ms

  @impl true
  def init(opts) do
    server = Keyword.get(opts, :name, __MODULE__)
    build = Keyword.get(opts, :build, &AetherHub.Directory.snapshot/0)
    now = Keyword.get(opts, :now, fn -> System.monotonic_time(:millisecond) end)

    ttl =
      Keyword.get(opts, :ttl, fn ->
        Application.get_env(:aether_hub, :directory_cache_ms, @default_ttl)
      end)

    :ets.new(server, [:named_table, :protected, :set, read_concurrency: true])
    :ets.insert(server, {:config, build, ttl, now})
    {:ok, server}
  end

  @impl true
  def handle_call(:refresh, _from, server) do
    [{:config, build, ttl, now}] = :ets.lookup(server, :config)

    # A reader that waited behind a build finds its value here.
    case fresh(server, ttl(ttl), now) do
      nil ->
        try do
          value = build.()
          :ets.insert(server, {:value, now.(), value})
          {:reply, {:ok, value}, server}
        catch
          kind, reason -> {:reply, {:error, kind, reason, __STACKTRACE__}, server}
        end

      value ->
        {:reply, {:ok, value}, server}
    end
  end

  def handle_call(:clear, _from, server) do
    :ets.delete(server, :value)
    {:reply, :ok, server}
  end
end
