defmodule AetherHub.Repo.Migrations.CreateAccountsAndInvites do
  use Ecto.Migration

  def change do
    create table(:accounts, primary_key: false) do
      add :endpoint, :string, primary_key: true
      add :handle, :string, null: false
      add :version, :integer, null: false, default: 0
    end

    create unique_index(:accounts, [:handle])

    create table(:invites, primary_key: false) do
      add :id, :string, primary_key: true
      add :sender_endpoint, references(:accounts, column: :endpoint, type: :string), null: false
      add :receiver_endpoint, references(:accounts, column: :endpoint, type: :string), null: false
      add :days, :integer, null: false
      add :status, :string, null: false
      add :bundle, :text
      add :created_at, :integer, null: false
    end

    create unique_index(:invites, [:sender_endpoint, :receiver_endpoint])
    create index(:invites, [:receiver_endpoint])
    create index(:invites, [:created_at])
  end
end
