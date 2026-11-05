defmodule AetherHub.ReleasedName do
  @moduledoc """
  A name with its number that a key gave up. The tag never goes to another
  key, so a contact name cannot move to a stranger.
  """
  use Ecto.Schema

  @primary_key false
  schema "released_names" do
    field :name, :string
    field :number, :integer
    field :endpoint, :string
    field :kind, :string
    field :released_at, :integer
  end
end
