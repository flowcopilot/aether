defmodule AetherHub.Json do
  @moduledoc "Reads JSON from nodes."

  @doc """
  Decodes `text` as JSON, like `JSON.decode/1`, but refuses an object with a
  key two times: two readers of such an object can see two different values.
  """
  def decode(text) when is_binary(text) do
    case JSON.decode(text, :ok, object_push: &push/3, object_finish: &finish/2) do
      {value, :ok, ""} -> {:ok, value}
      _ -> :error
    end
  catch
    :duplicate_key -> :error
  end

  def decode(_text), do: :error

  defp push(key, value, pairs), do: [{key, value} | pairs]

  defp finish(pairs, old) do
    map = Map.new(pairs)
    if map_size(map) != length(pairs), do: throw(:duplicate_key)
    {map, old}
  end
end
