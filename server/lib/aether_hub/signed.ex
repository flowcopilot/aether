defmodule AetherHub.Signed do
  @moduledoc """
  The messages that a node signs with its endpoint key, and their check.
  Each message is its lines joined with `\\n`; the formats are in
  `protocol/hub/contract.json`, and `protocol/hub/vectors.json` pins them.
  """

  def request_message(method, path_and_query, time, body),
    do: lines(["aether-hub/v1", String.upcase(method), path_and_query, time, sha256(body)])

  def listing_message(endpoint, expires_at, bytes),
    do:
      lines(["aether-listing/v1", endpoint, "true", Integer.to_string(expires_at), sha256(bytes)])

  def owner_claim_message(personal_endpoint, service_handle, expires_at),
    do:
      lines([
        "aether-owner/v1",
        "claim",
        personal_endpoint,
        service_handle,
        Integer.to_string(expires_at)
      ])

  def owner_confirm_message(service_endpoint, personal_endpoint, expires_at),
    do:
      lines([
        "aether-owner/v1",
        "confirm",
        service_endpoint,
        personal_endpoint,
        Integer.to_string(expires_at)
      ])

  @doc """
  Whether `signature` (unpadded base64url) signs `message` with the key of
  `endpoint` (lowercase hex). A value that does not decode is not valid.
  """
  def valid?(endpoint, message, signature) when is_binary(endpoint) and is_binary(signature) do
    with {:ok, <<_::binary-size(32)>> = key} <- Base.decode16(endpoint, case: :lower),
         {:ok, <<_::binary-size(64)>> = raw} <- Base.url_decode64(signature, padding: false) do
      :crypto.verify(:eddsa, :none, message, raw, [key, :ed25519])
    else
      _ -> false
    end
  end

  def valid?(_endpoint, _message, _signature), do: false

  defp lines(lines), do: Enum.join(lines, "\n")

  defp sha256(bytes), do: Base.encode16(:crypto.hash(:sha256, bytes), case: :lower)
end
