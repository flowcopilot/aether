defmodule AetherHub.Repo.Migrations.AddOriginToInvites do
  use Ecto.Migration

  def change do
    # `invite`: a host offered access. `request`: a node asked a listed
    # service agent for access.
    alter table(:invites) do
      add :origin, :string, null: false, default: "invite"
    end
  end
end
