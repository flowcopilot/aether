defmodule AetherHub.Repo.Migrations.CreateListings do
  @moduledoc """
  Public listings of service agents, and their full-text index. The index
  row of a listing has the listing `id` as its `rowid`.
  """
  use Ecto.Migration

  def up do
    create table(:listings) do
      add :endpoint, references(:accounts, column: :endpoint, type: :string), null: false
      # The base64url text exactly as the node sent it.
      add :listing, :text, null: false
      add :signature, :string, null: false
      add :expires_at, :integer, null: false
      add :updated_at, :integer, null: false
    end

    create unique_index(:listings, [:endpoint])
    create index(:listings, [:expires_at])

    execute """
    CREATE VIRTUAL TABLE listing_search USING fts5(
      name, description, skills, tags,
      tokenize = 'unicode61 remove_diacritics 2',
      prefix = '2 3'
    )
    """
  end

  def down do
    execute "DROP TABLE listing_search"
    drop table(:listings)
  end
end
