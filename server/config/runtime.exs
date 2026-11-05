import Config

port = String.to_integer(System.get_env("PORT") || "4000")

database_path =
  case config_env() do
    :prod ->
      System.get_env("DATABASE_PATH") ||
        raise "DATABASE_PATH is required, for example /data/aether_hub.db"

    :test ->
      Path.join(System.tmp_dir!(), "aether_hub_test_#{System.os_time()}.db")

    _ ->
      System.get_env("DATABASE_PATH") || "aether_hub.db"
  end

config :aether_hub, AetherHub.Repo,
  database: database_path,
  # One connection: SQLite takes one writer at a time, and a long poll does
  # not hold a connection while it waits.
  pool_size: 1,
  journal_mode: :wal,
  busy_timeout: 5_000,
  default_transaction_mode: :immediate

new_accounts_per_ip_hour =
  String.to_integer(System.get_env("NEW_ACCOUNTS_PER_IP_HOUR") || "20")

config :aether_hub,
  port: port,
  start_server: config_env() != :test,
  # The proxies whose `x-forwarded-for` header names the client, as CIDR
  # ranges. Behind a proxy, set it before the deploy: without it, every
  # client has the proxy's address.
  trusted_proxies: System.get_env("TRUSTED_PROXIES") || "",
  rate_limits: %{new_account_ip: {new_accounts_per_ip_hour, 3_600}}

if config_env() == :test do
  config :logger, level: :warning
end
