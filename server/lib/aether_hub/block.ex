defmodule AetherHub.Block do
  @moduledoc "The owner does not see invites from the blocked endpoint."
  use Ecto.Schema

  @primary_key false
  schema "blocks" do
    field :owner_endpoint, :string
    field :blocked_endpoint, :string
    field :created_at, :integer
  end
end
