defmodule AetherHub.Contact do
  @moduledoc "One direction of a contact pair, made when an invite is delivered."
  use Ecto.Schema

  @primary_key false
  schema "contacts" do
    field :owner_endpoint, :string
    field :contact_endpoint, :string
  end
end
