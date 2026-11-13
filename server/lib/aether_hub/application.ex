defmodule AetherHub.Application do
  @moduledoc false

  use Application

  @impl true
  def start(_type, _args) do
    # A bad TRUSTED_PROXIES value raises here and stops the boot.
    AetherHub.ClientIp.put_trusted(
      AetherHub.ClientIp.parse_cidrs!(Application.get_env(:aether_hub, :trusted_proxies, ""))
    )

    children =
      [
        AetherHub.Repo,
        {Ecto.Migrator, repos: [AetherHub.Repo], skip: false},
        AetherHub.ReadRepo,
        {Registry, keys: :duplicate, name: AetherHub.Waiters},
        AetherHub.RateLimit,
        AetherHub.Presence,
        AetherHub.AccountCache,
        AetherHub.DirectoryCache,
        AetherHub.Sweeper
      ] ++ server()

    Supervisor.start_link(children, strategy: :one_for_one, name: AetherHub.Supervisor)
  end

  defp server do
    if Application.get_env(:aether_hub, :start_server, true) do
      # HTTP/2 is off: the hub needs only HTTP/1.1, and Bandit 1.12.5 has an
      # open HTTP/2 advisory (EEF-CVE-2026-105659) with no fixed release yet.
      [
        {Bandit,
         plug: AetherHub.Router,
         port: Application.fetch_env!(:aether_hub, :port),
         http_2_options: [enabled: false]}
      ]
    else
      []
    end
  end
end
