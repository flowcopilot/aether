defmodule AetherHub.Signature do
  @moduledoc "Ed25519 signatures of the endpoint keys."

  @doc """
  Whether `signature` (64 raw bytes) signs `message` with the raw Ed25519
  public key `key`.
  """
  def valid?(message, signature, key) when byte_size(signature) == 64 and byte_size(key) == 32 do
    :crypto.verify(:eddsa, :none, message, signature, [key, :ed25519])
  rescue
    _ -> false
  end

  def valid?(_message, _signature, _key), do: false
end
