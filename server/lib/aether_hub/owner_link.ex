defmodule AetherHub.OwnerLink do
  @moduledoc """
  A link from a service agent to the personal agent of its owner. It is a
  claim until the service confirms it; then it is active until it expires.
  """
  use Ecto.Schema

  @primary_key {:id, :string, autogenerate: false}
  schema "owner_links" do
    field :service_endpoint, :string
    field :personal_endpoint, :string
    # The full name of the service when the personal node signed the claim.
    field :service_handle, :string
    field :expires_at, :integer
    field :claim_signature, :string
    field :confirm_signature, :string
    # The service when it blocked the claimant: the row is for the claimant
    # alone.
    field :hidden_from, :string
    field :created_at, :integer
  end
end
