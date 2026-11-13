defmodule AetherHub.DirectoryCacheTest do
  @moduledoc """
  The directory cache serves a value for its time to live, and builds a new
  one once, however many readers ask for it at the same moment. Each test
  starts its own cache with a build that it counts and a clock that it sets.
  """
  use ExUnit.Case, async: true

  alias AetherHub.DirectoryCache

  @ttl 15_000

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

    Agent.update(mode, fn _ -> :ok end)
    assert DirectoryCache.get(cache.name) == %{"ok" => true}

    Agent.update(mode, fn _ -> :fail end)
    cache.advance.(@ttl)
    assert DirectoryCache.get(cache.name) == %{"ok" => true}
  end
end
