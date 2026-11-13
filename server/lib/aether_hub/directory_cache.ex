defmodule AetherHub.DirectoryCache do
  @moduledoc """
  Keeps the built directory for 15 seconds, and builds it once at a time.

  A read inside the 15 seconds takes the value from an ETS table and sends
  no message. The first read after that asks this process to build a new
  value. The other readers wait in its mailbox; each one finds the new value
  when its turn comes, so a crowd of readers makes one build, not one each.

  A build that fails starts a pause of 2 seconds (`:retry`). In the pause no
  reader asks for a build, and a reader that was already waiting does not
  start one either: with an old value they all get it, and without one they
  fail at once with `AetherHub.DirectoryCache.Unavailable`, instead of
  queueing a build each behind a database that does not answer. The first
  reader that fails gets the error of the build itself. A build that works
  ends the pause.

  `get/1` returns the value and `fetch/1` the value with its age in
  milliseconds, which the route sends as the `age` header. A value that was
  served after a failed build can be older than the time to live.

  `get/1` and `fetch/1` raise `Unavailable` when this process is not running
  (the table is gone while the supervisor restarts it), or when it does not
  answer in 30 seconds and there is no old value.

  The time to live is `config :aether_hub, :directory_cache_ms`, 15 seconds
  when it is not set. At `0` there is no cache: the caller builds the value
  itself, which is what the tests do, because they set the clock of their
  own process.

  `start_link/1` also takes `:name`, `:build` (a function with no argument),
  `:ttl` (milliseconds, or a function that returns them), `:retry`
  (milliseconds) and `:now` (a function that returns milliseconds), for a
  second cache with its own table.
  """
  use GenServer

  defmodule Unavailable do
    @moduledoc "The directory cannot be served now: the route answers 503."
    defexception message: "the directory cache is not available"
  end

  @default_ttl :timer.seconds(15)
  @default_retry :timer.seconds(2)
  @call_timeout :timer.seconds(30)

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: Keyword.get(opts, :name, __MODULE__))
  end

  @doc "The directory, at most the time to live old (see the module)."
  def get(server \\ __MODULE__) do
    {value, _age} = fetch(server)
    value
  end

  @doc "The directory and its age in milliseconds."
  def fetch(server \\ __MODULE__) do
    [{:config, build, ttl, now, _retry}] = lookup(server, :config)

    ttl = ttl(ttl)

    if ttl <= 0 do
      {build.(), 0}
    else
      case serve(server, ttl, now) do
        {:ok, served} ->
          served

        :unavailable ->
          raise Unavailable, message: "the directory is not built and a build failed"

        :build ->
          server |> refresh() |> unwrap(server, now)
      end
    end
  end

  @doc "Forgets the value and the pause: the next read builds a new one."
  def clear(server \\ __MODULE__), do: GenServer.call(server, :clear)

  # What a read does without a build: the fresh value, the old value while a
  # failed build pauses the next one, nothing to serve in that pause, or a
  # build. Used by the readers, and by the process for the readers that
  # waited in its mailbox.
  defp serve(server, ttl, now) do
    t = now.()
    value = value(server)

    case value do
      {built_at, value} when t - built_at < ttl ->
        {:ok, {value, t - built_at}}

      {built_at, value} ->
        if paused?(server, t), do: {:ok, {value, t - built_at}}, else: :build

      nil ->
        if paused?(server, t), do: :unavailable, else: :build
    end
  end

  defp value(server) do
    case lookup(server, :value) do
      [{:value, built_at, value}] -> {built_at, value}
      [] -> nil
    end
  end

  defp paused?(server, t) do
    [{:config, _build, _ttl, _now, retry}] = lookup(server, :config)

    case lookup(server, :failed_at) do
      [{:failed_at, failed_at}] -> t - failed_at < retry
      [] -> false
    end
  end

  defp refresh(server) do
    GenServer.call(server, :refresh, @call_timeout)
  catch
    # Stopped, restarting, or a build that takes longer than the timeout.
    :exit, _ -> :down
  end

  defp unwrap({:ok, value, age}, _server, _now), do: {value, age}
  defp unwrap(:unavailable, _server, _now), do: raise(Unavailable)

  defp unwrap(failed, server, now) do
    # The old value is better than an error page.
    case value(server) do
      {built_at, value} ->
        {value, now.() - built_at}

      nil ->
        case failed do
          {:error, kind, reason, stacktrace} -> :erlang.raise(kind, reason, stacktrace)
          :down -> raise Unavailable, message: "the directory cache did not answer"
        end
    end
  end

  # The table is gone while the process restarts.
  defp lookup(server, key) do
    :ets.lookup(server, key)
  rescue
    ArgumentError ->
      reraise Unavailable, [message: "the directory cache is not running"], __STACKTRACE__
  end

  defp ttl(fun) when is_function(fun, 0), do: fun.()
  defp ttl(ms) when is_integer(ms), do: ms

  @impl true
  def init(opts) do
    server = Keyword.get(opts, :name, __MODULE__)
    build = Keyword.get(opts, :build, &AetherHub.Directory.snapshot/0)
    now = Keyword.get(opts, :now, fn -> System.monotonic_time(:millisecond) end)
    retry = Keyword.get(opts, :retry, @default_retry)

    ttl =
      Keyword.get(opts, :ttl, fn ->
        Application.get_env(:aether_hub, :directory_cache_ms, @default_ttl)
      end)

    :ets.new(server, [:named_table, :protected, :set, read_concurrency: true])
    :ets.insert(server, {:config, build, ttl, now, retry})
    {:ok, server}
  end

  @impl true
  def handle_call(:refresh, _from, server) do
    [{:config, build, ttl, now, _retry}] = :ets.lookup(server, :config)

    # A reader that waited behind a build finds its value here, or the pause
    # that the failure of that build started.
    case serve(server, ttl(ttl), now) do
      {:ok, {value, age}} ->
        {:reply, {:ok, value, age}, server}

      :unavailable ->
        {:reply, :unavailable, server}

      :build ->
        try do
          value = build.()
          :ets.insert(server, {:value, now.(), value})
          :ets.delete(server, :failed_at)
          {:reply, {:ok, value, 0}, server}
        catch
          kind, reason ->
            :ets.insert(server, {:failed_at, now.()})
            {:reply, {:error, kind, reason, __STACKTRACE__}, server}
        end
    end
  end

  def handle_call(:clear, _from, server) do
    :ets.delete(server, :value)
    :ets.delete(server, :failed_at)
    {:reply, :ok, server}
  end
end
