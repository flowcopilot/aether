defmodule AetherHub.Account do
  @moduledoc """
  A hub account: one Iroh endpoint key, one agent. Its full name is
  `name#NNNN`. `kind` is `personal` or `service` and does not change.
  `version` counts the changes that the endpoint's long poll waits for.
  """
  use Ecto.Schema

  @primary_key {:endpoint, :string, autogenerate: false}
  schema "accounts" do
    field :name, :string
    field :number, :integer
    field :kind, :string, default: "personal"
    field :searchable, :boolean, default: false
    field :created_at, :integer, default: 0
    field :version, :integer, default: 0
  end

  def full_name(%__MODULE__{name: name, number: number}), do: AetherHub.Names.format(name, number)
end
