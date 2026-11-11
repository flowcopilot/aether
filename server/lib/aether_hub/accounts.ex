defmodule AetherHub.Accounts do
  @moduledoc """
  Accounts and what goes with them. A rename and a deletion end the rows
  that name the agent; a block ends what two nodes shared. Each cascade is
  listed here, in one place.
  """

  alias AetherHub.{Account, Blocks, Clock, Contacts, Invites, Listings, Names, OwnerLinks}
  alias AetherHub.{Protocol, Repo, Versions}

  @kinds Protocol.kinds()
  @service Protocol.kind(:service)
  @account_keys ["name", "kind", "searchable"]

  def get(endpoint), do: Repo.get(Account, endpoint)

  def json(%Account{} = account) do
    %{
      "handle" => Account.full_name(account),
      "name" => account.name,
      "number" => account.number,
      "endpoint" => account.endpoint,
      "kind" => account.kind,
      "searchable" => account.searchable
    }
  end

  @doc """
  The rate-limit bucket and key that a `PUT /v1/account` with `body` counts
  against: a first claim counts for the client address, a rename for the
  endpoint, and another change for nothing.
  """
  def quota(nil, _body, client_ip), do: {:new_account_ip, client_ip}

  def quota(%Account{} = account, %{"name" => name}, _client_ip) when name != account.name,
    do: {:rename, account.endpoint}

  def quota(%Account{}, _body, _client_ip), do: nil

  @doc """
  Claims an account for `endpoint` (`account` is `nil`), or changes the
  name or the search setting of `account`. A rename keeps the current
  number when it is free for the new name.
  """
  def put(endpoint, account, %{} = body) do
    with :ok <- valid_body(body) do
      case account do
        nil -> claim(endpoint, body)
        %Account{} -> change(account, body)
      end
    end
  end

  def put(_endpoint, _account, _body), do: {:error, :invalid}

  defp valid_body(body) do
    valid =
      Enum.all?(Map.keys(body), &(&1 in @account_keys)) and
        (not Map.has_key?(body, "name") or Names.valid_name?(body["name"])) and
        (not Map.has_key?(body, "kind") or body["kind"] in @kinds) and
        (not Map.has_key?(body, "searchable") or is_boolean(body["searchable"]))

    if valid, do: :ok, else: {:error, :invalid}
  end

  defp claim(endpoint, body) do
    kind = Map.get(body, "kind", Protocol.default_kind())
    searchable = Map.get(body, "searchable", false)

    cond do
      not Map.has_key?(body, "name") ->
        {:error, :invalid}

      kind == @service and searchable ->
        {:error, :invalid}

      # The type of an agent does not change, also after a deletion.
      Names.released_with_other_kind?(endpoint, kind) ->
        {:error, :invalid}

      true ->
        Repo.atomic(fn ->
          name = body["name"]

          Repo.insert!(%Account{
            endpoint: endpoint,
            name: name,
            number: number!(name, endpoint, Names.released_numbers(name, endpoint)),
            kind: kind,
            searchable: searchable,
            created_at: Clock.now(),
            version: 1
          })
          |> json()
        end)
    end
  end

  defp change(%Account{} = account, body) do
    name = Map.get(body, "name", account.name)
    searchable = Map.get(body, "searchable", account.searchable)

    cond do
      Map.get(body, "kind", account.kind) != account.kind ->
        {:error, :invalid}

      account.kind == @service and searchable ->
        {:error, :invalid}

      name != account.name ->
        rename(account, name, searchable)

      true ->
        {:ok,
         account |> Ecto.Changeset.change(searchable: searchable) |> Repo.update!() |> json()}
    end
  end

  defp rename(%Account{endpoint: endpoint} = account, name, searchable) do
    Versions.commit(fn ->
      number = number!(name, endpoint, [account.number | Names.released_numbers(name, endpoint)])
      Names.release(account.name, account.number, endpoint, account.kind)
      end_named_rows(endpoint)

      renamed =
        account
        |> Ecto.Changeset.change(name: name, number: number, searchable: searchable)
        |> Repo.update!()

      {json(renamed), [endpoint | Invites.counterparts(endpoint)]}
    end)
  end

  defp number!(name, endpoint, preferred) do
    case Names.pick_number(name, endpoint, preferred) do
      {:ok, number} -> number
      {:error, :taken} -> Repo.rollback(:taken)
    end
  end

  # A listing and an owner link name the agent, so they end with the name.
  defp end_named_rows(endpoint) do
    with {:error, reason} <- Listings.delete(endpoint), do: Repo.rollback(reason)
    OwnerLinks.delete_for(endpoint)
  end

  @doc """
  Deletes `account`, its invites, its contacts and the rows that name it.
  Its tag stays with the key, with its type; blocks stay too.
  """
  def delete(%Account{endpoint: endpoint} = account) do
    Versions.commit(fn ->
      touched = Enum.uniq(Invites.counterparts(endpoint) ++ Contacts.of(endpoint))
      Names.release(account.name, account.number, endpoint, account.kind)
      Invites.delete_for(endpoint)
      Contacts.delete_for(endpoint)
      end_named_rows(endpoint)
      Repo.delete!(account)
      {:ok, touched}
    end)
  end

  ## Blocks

  @doc """
  `account` blocks `blocked`: the open invites between the two, their
  contact pair and their owner links go, and later rows from `blocked`
  exist for it alone.
  """
  def block(%Account{endpoint: endpoint}, blocked) do
    with :ok <- Protocol.validate_endpoint(blocked),
         false <- blocked == endpoint do
      Versions.commit(fn ->
        with {:error, reason} <- Blocks.put(endpoint, blocked), do: Repo.rollback(reason)
        Invites.delete_between(endpoint, blocked)
        Contacts.delete_between(endpoint, blocked)
        OwnerLinks.delete_between(endpoint, blocked)
        {:ok, [endpoint, blocked]}
      end)
    else
      {:error, :invalid} -> {:error, :invalid}
      true -> {:error, :invalid}
    end
  end

  @doc "Blocks the other side of the open row `id`."
  def block_through(%Account{} = account, id) do
    with {:ok, other} <- Invites.block_target(account, id), do: block(account, other)
  end

  def unblock(%Account{endpoint: endpoint}, blocked) do
    with :ok <- Protocol.validate_endpoint(blocked),
         :ok <- Blocks.delete(endpoint, blocked),
         do: :ok
  end

  def blocks(%Account{endpoint: endpoint}), do: {:ok, %{"blocks" => Blocks.list(endpoint)}}
end
