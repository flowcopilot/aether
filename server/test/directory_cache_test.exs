defmodule AetherHub.DirectoryCacheTest do
  @moduledoc """
  The directory cache serves a value for its time to live, and builds a new
  one once, however many readers ask for it at the same moment. Each test
  starts its own cache with a build that it counts and a clock that it sets.
  """
  use ExUnit.Case, async: true

  alias AetherHub.DirectoryCache

  @ttl 15_000
  @retry 2_000

  # A cache whose build counts its calls, with a clock in milliseconds.
  defp start_cache(opts \\ []) do
    name = :"directory_cache_#{System.unique_integer([:positive])}"
    {:ok, counter} = Agent.start_link(fn -> 0 end)
    {:ok, clock} = Agent.start_link(fn -> 0 end)

    build =
      Keyword.get(opts, :build, fn ->
        Process.sleep(Keyword.get(opts, :build_ms, 0))
        n = Agent.get_and_update(counter, &{&1 + 1, &1 + 1})
        %{"build" => n}
      end)

    start_supervised!(%{
      id: name,
      start:
        {DirectoryCache, :start_link,
         [
           [
             name: name,
             build: build,
             ttl: Keyword.get(opts, :ttl, @ttl),
             retry: @retry,
             now: fn -> Agent.get(clock, & &1) end
           ]
         ]}
    })

    %{
      name: name,
      builds: fn -> Agent.get(counter, & &1) end,
      advance: fn ms -> Agent.update(clock, &(&1 + ms)) end
    }
  end

  test "serves the same value for 15 seconds, then builds a new one" do
    cache = start_cache()

    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    cache.advance.(@ttl - 1)
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    assert cache.builds.() == 1

    cache.advance.(1)
    assert DirectoryCache.get(cache.name) == %{"build" => 2}
    assert DirectoryCache.get(cache.name) == %{"build" => 2}
    assert cache.builds.() == 2
  end

  test "readers at the same moment make one build" do
    cache = start_cache(build_ms: 100)

    values =
      1..50
      |> Enum.map(fn _ -> Task.async(fn -> DirectoryCache.get(cache.name) end) end)
      |> Task.await_many()

    assert Enum.uniq(values) == [%{"build" => 1}]
    assert cache.builds.() == 1
  end

  test "readers after the time to live make one build" do
    cache = start_cache(build_ms: 100)
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    cache.advance.(@ttl)

    values =
      1..50
      |> Enum.map(fn _ -> Task.async(fn -> DirectoryCache.get(cache.name) end) end)
      |> Task.await_many()

    assert Enum.uniq(values) == [%{"build" => 2}]
    assert cache.builds.() == 2
  end

  test "clear makes the next read build a new value" do
    cache = start_cache()
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    :ok = DirectoryCache.clear(cache.name)
    assert DirectoryCache.get(cache.name) == %{"build" => 2}
  end

  test "a time to live of 0 builds in the process of the reader, every time" do
    cache = start_cache(ttl: 0)
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    assert DirectoryCache.get(cache.name) == %{"build" => 2}
  end

  test "a failed build serves the old value, and raises when there is none" do
    {:ok, mode} = Agent.start_link(fn -> :fail end)

    cache =
      start_cache(
        build: fn ->
          if Agent.get(mode, & &1) == :fail, do: raise("database is busy"), else: %{"ok" => true}
        end
      )

    assert_raise RuntimeError, "database is busy", fn -> DirectoryCache.get(cache.name) end

    # The failure paused the next build for a short time.
    Agent.update(mode, fn _ -> :ok end)
    cache.advance.(@retry)
    assert DirectoryCache.get(cache.name) == %{"ok" => true}

    Agent.update(mode, fn _ -> :fail end)
    cache.advance.(@ttl)
    assert DirectoryCache.get(cache.name) == %{"ok" => true}
  end

  test "fetch tells the age of the value in milliseconds" do
    cache = start_cache()

    assert DirectoryCache.fetch(cache.name) == {%{"build" => 1}, 0}
    cache.advance.(5_000)
    assert DirectoryCache.fetch(cache.name) == {%{"build" => 1}, 5_000}
  end

  # A build that counts its attempts, and works or fails as `mode` says.
  defp flaky_cache(build_ms \\ 0) do
    {:ok, mode} = Agent.start_link(fn -> :ok end)
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    cache =
      start_cache(
        build: fn ->
          Process.sleep(build_ms)
          n = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          if Agent.get(mode, & &1) == :fail, do: raise("database is busy"), else: %{"build" => n}
        end
      )

    Map.merge(cache, %{
      attempts: fn -> Agent.get(attempts, & &1) end,
      mode: fn value -> Agent.update(mode, fn _ -> value end) end
    })
  end

  defp readers(cache, count) do
    1..count
    |> Enum.map(fn _ ->
      Task.async(fn ->
        try do
          {:ok, DirectoryCache.get(cache.name)}
        rescue
          error -> {:error, error}
        end
      end)
    end)
    |> Task.await_many()
  end

  test "after a failed build the old value is served without a new build for a while" do
    cache = flaky_cache()
    assert DirectoryCache.get(cache.name) == %{"build" => 1}

    cache.mode.(:fail)
    cache.advance.(@ttl)

    # The first read after the time to live tries, and fails.
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    assert cache.attempts.() == 2

    for _ <- 1..20, do: assert(DirectoryCache.get(cache.name) == %{"build" => 1})
    cache.advance.(@retry - 1)
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    assert cache.attempts.() == 2

    # The pause ends: one more try, which fails again and pauses again.
    cache.advance.(1)
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    assert cache.attempts.() == 3
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    assert cache.attempts.() == 3

    # A try that works ends the failures.
    cache.mode.(:ok)
    cache.advance.(@retry)
    assert DirectoryCache.get(cache.name) == %{"build" => 4}
    assert cache.attempts.() == 4
    assert DirectoryCache.get(cache.name) == %{"build" => 4}
  end

  test "without an old value the first read gets the error and the next ones fail at once" do
    cache = flaky_cache()
    cache.mode.(:fail)

    assert_raise RuntimeError, "database is busy", fn -> DirectoryCache.get(cache.name) end
    assert cache.attempts.() == 1

    for _ <- 1..5 do
      assert_raise DirectoryCache.Unavailable, fn -> DirectoryCache.get(cache.name) end
    end

    assert cache.attempts.() == 1

    # After the pause there is one more try, and when it works the cache serves.
    cache.mode.(:ok)
    cache.advance.(@retry)
    assert DirectoryCache.get(cache.name) == %{"build" => 2}
  end

  test "readers that wait behind a slow failing build do not build again" do
    cache = flaky_cache(100)
    assert DirectoryCache.get(cache.name) == %{"build" => 1}
    cache.mode.(:fail)
    cache.advance.(@ttl)

    results = readers(cache, 50)

    assert Enum.uniq(results) == [{:ok, %{"build" => 1}}]
    assert cache.attempts.() == 2
  end

  test "without an old value, readers behind a failing build get one error and the rest fail at once" do
    cache = flaky_cache(100)
    cache.mode.(:fail)

    results = readers(cache, 50)

    assert [%RuntimeError{message: "database is busy"}] =
             for({:error, %RuntimeError{} = error} <- results, do: error)

    assert length(for({:error, %DirectoryCache.Unavailable{}} <- results, do: :unavailable)) == 49
    assert cache.attempts.() == 1
  end

  test "a read while the cache process is down is unavailable, not an ArgumentError" do
    cache = start_cache()
    assert DirectoryCache.get(cache.name) == %{"build" => 1}

    :ok = stop_supervised(cache.name)

    assert_raise DirectoryCache.Unavailable, fn -> DirectoryCache.get(cache.name) end
    assert_raise DirectoryCache.Unavailable, fn -> DirectoryCache.fetch(cache.name) end

    assert_raise DirectoryCache.Unavailable, fn ->
      DirectoryCache.get(:no_such_directory_cache)
    end
  end
end
