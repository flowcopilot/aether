defmodule AetherHub.Repo do
  use Ecto.Repo, otp_app: :aether_hub, adapter: Ecto.Adapters.SQLite3

  @doc """
  Runs `fun` in a transaction: `{:ok, value}` with its value, or
  `{:error, reason}` after `rollback(reason)`.
  """
  def atomic(fun), do: transact(fn -> {:ok, fun.()} end)

  @doc "Inside `atomic/1`: the value of `{:ok, value}`, or `rollback(reason)` for `{:error, reason}`."
  def ok!({:ok, value}), do: value
  def ok!({:error, reason}), do: rollback(reason)

  @doc "A new row id: 32 lowercase hex characters."
  def new_id, do: Base.encode16(:crypto.strong_rand_bytes(16), case: :lower)
end
