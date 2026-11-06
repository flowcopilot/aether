defmodule AetherHub.Clock do
  @moduledoc """
  The hub's clock, in Unix seconds. A process can set its own time with
  `set/1`: a test then makes rows in the past or reads them later without
  changing rows by hand, and other processes keep the real time.
  """

  @key {__MODULE__, :now}

  def now, do: Process.get(@key) || System.system_time(:second)

  @doc """
  Sets the time of the calling process to the Unix time `time`, or back to
  the real time with `nil`. Returns the time that was set before, or `nil`.
  """
  def set(nil), do: Process.delete(@key)
  def set(time) when is_integer(time), do: Process.put(@key, time)
end
