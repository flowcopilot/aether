defmodule AetherHub.Repo.Migrations.CreateContacts do
  use Ecto.Migration

  def change do
    create table(:contacts, primary_key: false) do
      add :owner_endpoint, :string, null: false
      add :contact_endpoint, :string, null: false
    end

    create unique_index(:contacts, [:owner_endpoint, :contact_endpoint])
    create index(:contacts, [:contact_endpoint])
  end
end
