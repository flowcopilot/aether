defmodule AetherHub.Listing do
  @moduledoc """
  The public listing of a service agent. `listing` is the base64url text of
  the listing bytes, kept exactly as the node sent it, and `signature` signs
  those bytes with the endpoint key.
  """
  use Ecto.Schema

  schema "listings" do
    field :endpoint, :string
    field :listing, :string
    field :signature, :string
    field :expires_at, :integer
    field :updated_at, :integer
  end
end
