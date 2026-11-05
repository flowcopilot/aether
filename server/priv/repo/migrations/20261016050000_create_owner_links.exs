defmodule AetherHub.Repo.Migrations.CreateOwnerLinks do
  use Ecto.Migration

  def change do
    # A service agent linked to the personal agent of its owner. The
    # personal node signs the claim; the service node signs the confirm.
    # No foreign key: a rename or a deletion removes the links in code.
    create table(:owner_links, primary_key: false) do
      add :id, :string, primary_key: true
      add :service_endpoint, :string, null: false
      add :personal_endpoint, :string, null: false
      add :service_handle, :string, null: false
      add :expires_at, :integer, null: false
      add :claim_signature, :string, null: false
      add :confirm_signature, :string
      add :hidden_from, :string
      add :created_at, :integer, null: false
    end

    create unique_index(:owner_links, [:service_endpoint, :personal_endpoint])
    create index(:owner_links, [:personal_endpoint])
  end
end
