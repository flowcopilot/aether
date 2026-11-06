defmodule AetherHub.Params do
  @moduledoc "Reads query parameters."

  @limits AetherHub.Protocol.limit(:search_limit)
  @default_limit AetherHub.Protocol.limit(:search_default_limit)

  @doc "The `limit` of a search, or the default when it is absent."
  def limit(nil), do: {:ok, @default_limit}
  def limit(value), do: integer(value, @limits.first, @limits.last)

  @doc "An optional integer from `min` to `max` (`nil` for no maximum)."
  def integer(nil, _min, _max), do: {:ok, nil}

  def integer(value, min, max) when is_binary(value) do
    case Integer.parse(value) do
      {n, ""} when n >= min and (max == nil or n <= max) -> {:ok, n}
      _ -> :error
    end
  end

  def integer(_value, _min, _max), do: :error
end
