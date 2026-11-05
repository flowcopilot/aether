defmodule AetherHub.Sweeper do
  @moduledoc """
  Once a minute, removes expired invites and wakes both sides, and removes
  expired listings and owner links.
  """
  use GenServer

  @interval 60_000

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  @impl true
  def init(nil) do
    schedule()
    {:ok, nil}
  end

  @impl true
  def handle_info(:sweep, state) do
    AetherHub.Hub.delete_expired()
    AetherHub.Listings.delete_expired()
    AetherHub.OwnerLinks.delete_expired()
    schedule()
    {:noreply, state}
  end

  defp schedule, do: Process.send_after(self(), :sweep, @interval)
end
