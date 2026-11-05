defmodule AetherHub.Sweeper do
  @moduledoc "Removes what expired, once a minute."
  use GenServer

  require Logger

  @interval :timer.minutes(1)

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  def sweep do
    for {name, sweep} <- [
          invites: &AetherHub.Invites.delete_expired/0,
          listings: &AetherHub.Listings.delete_expired/0,
          owner_links: &AetherHub.OwnerLinks.delete_expired/0,
          presence: &AetherHub.Presence.sweep/0,
          rate_limits: &AetherHub.RateLimit.sweep/0
        ] do
      with {:error, reason} <- sweep.() do
        Logger.error("the sweep of #{name} failed: #{inspect(reason)}")
      end
    end

    :ok
  end

  @impl true
  def init(nil) do
    schedule()
    {:ok, nil}
  end

  @impl true
  def handle_info(:sweep, state) do
    sweep()
    schedule()
    {:noreply, state}
  end

  defp schedule, do: Process.send_after(self(), :sweep, @interval)
end
