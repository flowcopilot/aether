database = Application.fetch_env!(:aether_hub, AetherHub.Repo)[:database]

ExUnit.after_suite(fn _ ->
  for suffix <- ["", "-wal", "-shm"], do: File.rm(database <> suffix)
end)

# The scenarios read the directory right after they change it, in a process
# with its own clock: they run without the 15 second cache. Its own test
# turns the cache on.
Application.put_env(:aether_hub, :directory_cache_ms, 0)

ExUnit.start()
