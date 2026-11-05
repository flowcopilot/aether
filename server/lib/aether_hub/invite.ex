defmodule AetherHub.Invite do
  @moduledoc """
  An invite from a host (sender) to another node (receiver). For a request
  (`origin` `request`), the receiver asked the listed service agent (the
  sender) for access; the service signs the line, so it is the sender.
  """
  use Ecto.Schema

  @primary_key {:id, :string, autogenerate: false}
  schema "invites" do
    field :sender_endpoint, :string
    field :receiver_endpoint, :string
    field :days, :integer
    field :status, :string
    field :bundle, :string
    field :created_at, :integer
    # The receiver when it blocked the sender: the row exists for the
    # sender only.
    field :hidden_from, :string
    field :origin, :string, default: "invite"
  end
end
