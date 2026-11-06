defmodule AetherHub.Contract do
  @moduledoc """
  The hub contract and signing vectors in `protocol/hub/` at the repository
  root. Tests build and sign messages from these files, not from the hub's
  own code, so a change of format on one side fails here.
  """

  @dir Path.expand("../../../protocol/hub", __DIR__)
  @contract_path Path.join(@dir, "contract.json")
  @vectors_path Path.join(@dir, "vectors.json")
  @external_resource @contract_path
  @external_resource @vectors_path
  @contract @contract_path |> File.read!() |> JSON.decode!()
  @vectors @vectors_path |> File.read!() |> JSON.decode!()

  def contract, do: @contract
  def vectors, do: @vectors

  @doc "The contract value at `path`, a list of keys."
  def get(path), do: get_in(@contract, path)

  @doc "The keys of the contract object `name`; `contact: true` adds the contact keys."
  def keys(name, opts \\ []) do
    object = @contract["objects"][name]
    extra = if Keyword.get(opts, :contact, false), do: object["contactKeys"], else: []
    Enum.sort(object["keys"] ++ extra)
  end

  @doc """
  The message `kind` (such as `"request"`) built from `fields`, a map from
  the contract's field names to values. A `...Sha256` field that is absent
  is the hash of its source: `body` for a request, `listingBytes` for a
  listing.
  """
  def message(kind, fields) do
    fields =
      fields
      |> with_hash("bodySha256", "body")
      |> with_hash("listingSha256", "listingBytes")

    @contract["signing"]["messages"][kind]
    |> Enum.map(fn line ->
      Regex.replace(~r/\{(\w+)\}/, line, fn _, key -> to_text(Map.fetch!(fields, key)) end)
    end)
    |> Enum.join("\n")
  end

  defp with_hash(fields, hash, source) do
    case fields do
      %{^hash => _} -> fields
      %{^source => bytes} -> Map.put(fields, hash, sha256(bytes))
      _ -> fields
    end
  end

  defp to_text(value) when is_integer(value), do: Integer.to_string(value)
  defp to_text(value) when is_binary(value), do: value

  defp sha256(bytes), do: Base.encode16(:crypto.hash(:sha256, bytes), case: :lower)

  @doc "The unpadded base64url Ed25519 signature of `message` with the 32-byte `seed`."
  def sign(seed, message) do
    :crypto.sign(:eddsa, :none, message, [seed, :ed25519]) |> Base.url_encode64(padding: false)
  end
end
