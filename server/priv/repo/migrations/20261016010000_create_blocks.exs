defmodule AetherHub.Repo.Migrations.CreateBlocks do
  use Ecto.Migration

  def change do
    # No foreign key: a block stays when either account is deleted.
    create table(:blocks, primary_key: false) do
      add :owner_endpoint, :string, null: false
      add :blocked_endpoint, :string, null: false
      add :created_at, :integer, null: false
    end

    create unique_index(:blocks, [:owner_endpoint, :blocked_endpoint])

    # An invite from a blocked sender exists for the sender only.
    alter table(:invites) do
      add :hidden_from, :string
    end
  end
end
