defmodule AetherHub.Notify do
  @moduledoc """
  Wake-ups for long polls. A waiting request registers under its endpoint in
  `AetherHub.Waiters`; every write sends a message to each waiter of every
  endpoint it touched.
  """

  @registry AetherHub.Waiters
  @message :aether_hub_changed

  def subscribe(endpoint) do
    {:ok, _} = Registry.register(@registry, endpoint, nil)
    :ok
  end

  @doc "Removes the registration and drops any wake-up already in the mailbox."
  def unsubscribe(endpoint) do
    Registry.unregister(@registry, endpoint)
    flush()
  end

  @doc "Blocks until a wake-up arrives or `timeout_ms` passes."
  def await(timeout_ms) do
    receive do
      {@message, _endpoint} -> :changed
    after
      timeout_ms -> :timeout
    end
  end

  @doc "Wakes every waiter of each endpoint in the list."
  def changed(endpoints) do
    for endpoint <- Enum.uniq(endpoints) do
      Registry.dispatch(@registry, endpoint, fn entries ->
        for {pid, _} <- entries, do: send(pid, {@message, endpoint})
      end)
    end

    :ok
  end

  defp flush do
    receive do
      {@message, _} -> flush()
    after
      0 -> :ok
    end
  end
end
