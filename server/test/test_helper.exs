database = Application.fetch_env!(:aether_hub, AetherHub.Repo)[:database]

ExUnit.after_suite(fn _ ->
  for suffix <- ["", "-wal", "-shm"], do: File.rm(database <> suffix)
end)

ExUnit.start()
