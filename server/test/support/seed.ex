defmodule AetherHub.Seed do
  @moduledoc """
  Rows for a test that needs many accounts and listings: they go straight
  into the database, with no signed request, as the hub would have kept
  them. A listing has its search row, and an owner link is kept as it is
  after the service confirmed it.
  """

  alias AetherHub.{Account, Clock, Listing, OwnerLink, Presence, Repo}

  @doc "The endpoint of a new key: 64 lowercase hex characters."
  def endpoint, do: Base.encode16(:crypto.strong_rand_bytes(32), case: :lower)

  @doc "A personal account. Returns the `%Account{}`."
  def personal(name \\ AetherHub.TestClient.unique_name()), do: account(name, "personal")

  defp account(name, kind) do
    Repo.insert!(%Account{
      endpoint: endpoint(),
      name: name,
      number: Enum.random(1..9999),
      kind: kind,
      created_at: Clock.now(),
      version: 1
    })
  end

  @doc """
  A service agent with a listing. Options:

    * `:description`, `:skill`, `:tags` - the text of the listing
    * `:updated_at` - when the node sent it (default: now)
    * `:expires_at` - default: in 7 days
    * `:online` - the agent has a recent request (default: `false`)
    * `:owner` - `:active` (confirmed), `:claim` (not confirmed) or
      `:expired` (confirmed, then ended): the link to a new personal account
    * `:bad` - the listing text does not read
  """
  def service(opts \\ []) do
    now = Clock.now()
    account = account(AetherHub.TestClient.unique_name(), "service")
    handle = Account.full_name(account)
    description = Keyword.get(opts, :description, "Answers questions.")
    skill = Keyword.get(opts, :skill, "General")
    tags = Keyword.get(opts, :tags, ["general"])

    text =
      if opts[:bad] do
        "not base64url!"
      else
        %{
          "name" => handle,
          "description" => description,
          "skills" => [
            %{"id" => "general", "name" => skill, "description" => "Does tasks.", "tags" => tags}
          ]
        }
        |> JSON.encode!()
        |> Base.url_encode64(padding: false)
      end

    listing =
      Repo.insert!(%Listing{
        endpoint: account.endpoint,
        listing: text,
        signature: "signature",
        expires_at: Keyword.get(opts, :expires_at, now + 7 * 86_400),
        updated_at: Keyword.get(opts, :updated_at, now)
      })

    Repo.query!(
      "INSERT INTO listing_search(rowid, name, description, skills, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
      [listing.id, account.name, description, skill <> "\nDoes tasks.", Enum.join(tags, " ")]
    )

    if opts[:online], do: Presence.seen(account.endpoint)
    owner = if opts[:owner], do: link(account, handle, opts[:owner], now)

    # `owner` is the owner that the hub shows: only an active link has one.
    %{account: account, handle: handle, owner: if(opts[:owner] == :active, do: owner)}
  end

  defp link(service, handle, state, now) do
    personal = personal()

    Repo.insert!(%OwnerLink{
      id: Repo.new_id(),
      service_endpoint: service.endpoint,
      personal_endpoint: personal.endpoint,
      service_handle: handle,
      expires_at: if(state == :expired, do: now - 10, else: now + 86_400),
      claim_signature: "claim",
      confirm_signature: if(state == :claim, do: nil, else: "confirm"),
      created_at: now
    })

    Account.full_name(personal)
  end

  @doc "`asker` has `contact` as a contact."
  def contact(asker, contact) do
    Repo.insert_all(AetherHub.Contact, [
      %{owner_endpoint: asker.endpoint, contact_endpoint: contact.endpoint}
    ])
  end
end
