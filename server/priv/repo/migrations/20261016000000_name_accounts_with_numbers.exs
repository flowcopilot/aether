defmodule AetherHub.Repo.Migrations.NameAccountsWithNumbers do
  @moduledoc """
  Hub names become `name#NNNN`. Each old handle keeps its text as the name and
  gets a random number. A released name with its number never goes to
  another key, so `released_names` keeps every tag that a key gave up.

  Raw SQL only: a later change to the schema modules must not change what
  this migration does.
  """
  use Ecto.Migration

  def up do
    execute "DROP INDEX IF EXISTS accounts_handle_index"
    execute "ALTER TABLE accounts RENAME COLUMN handle TO name"
    execute "ALTER TABLE accounts ADD COLUMN number INTEGER NOT NULL DEFAULT 0"
    execute "ALTER TABLE accounts ADD COLUMN kind TEXT NOT NULL DEFAULT 'personal'"
    execute "ALTER TABLE accounts ADD COLUMN searchable INTEGER NOT NULL DEFAULT 0"
    execute "ALTER TABLE accounts ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0"
    flush()

    now = System.system_time(:second)
    %{rows: rows} = repo().query!("SELECT endpoint FROM accounts")

    # The old handles were unique, so each name has one row, and any number
    # is free for it.
    for [endpoint] <- rows do
      repo().query!(
        "UPDATE accounts SET number = ?1, version = version + 1, created_at = ?2 WHERE endpoint = ?3",
        [Enum.random(1..9999), now, endpoint]
      )
    end

    execute "CREATE UNIQUE INDEX accounts_name_number_index ON accounts(name, number)"

    execute """
    CREATE TABLE released_names (
      name TEXT NOT NULL,
      number INTEGER NOT NULL,
      endpoint TEXT NOT NULL,
      kind TEXT NOT NULL,
      released_at INTEGER NOT NULL
    )
    """

    execute "CREATE UNIQUE INDEX released_names_name_number_index ON released_names(name, number)"
    execute "CREATE INDEX released_names_endpoint_index ON released_names(endpoint)"
  end

  def down do
    raise "This migration is one-way: restore the database backup."
  end
end
