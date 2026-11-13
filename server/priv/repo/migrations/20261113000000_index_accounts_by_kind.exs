defmodule AetherHub.Repo.Migrations.IndexAccountsByKind do
  @moduledoc """
  The directory counts the personal accounts. Without an index on `kind`
  that count reads the whole `accounts` table. The index is built once, in
  one statement, and `IF NOT EXISTS` makes a second run harmless.

  Raw SQL only: a later change to the schema modules must not change what
  this migration does.
  """
  use Ecto.Migration

  def up do
    execute "CREATE INDEX IF NOT EXISTS accounts_kind_index ON accounts(kind)"
  end

  def down do
    execute "DROP INDEX IF EXISTS accounts_kind_index"
  end
end
