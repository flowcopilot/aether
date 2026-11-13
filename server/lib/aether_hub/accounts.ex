defmodule AetherHub.Accounts do
  @moduledoc """
  Accounts and what goes with them. A rename and a deletion end the rows
  that name the agent; a block ends what two nodes shared. Each cascade is
  listed here, in one place.
  """

  alias AetherHub.{Account, AccountCache, Blocks, Clock, Contacts, Invites, Listings, Names}
  alias AetherHub.{OwnerLinks, Protocol, Repo, Versions}

  @kinds Protocol.kinds()
  @service Protocol.kind(:service)
  @account_keys ["name", "kind", "searchable"]

  def get(endpoint), do: Repo.get(Account, endpoint)

  @doc """
  The account that a signed request acts as, from `AetherHub.AccountCache`.
  Every write to the `accounts` table in this module invalidates the cache.
  """
  def fetch(endpoint), do: AccountCache.fetch(endpoint)

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
  def put(endpoint, account, body) do
    result = write(endpoint, account, body)
    AccountCache.invalidate(endpoint)
    result
  end

  defp write(endpoint, account, %{} = body) do
    with :ok <- valid_body(body) do
      # The values after the change: the body over the current ones.
      wanted = Map.merge(current(account), body)

      cond do
        # A service agent is never in the people search.
        wanted["kind"] == @service and wanted["searchable"] -> {:error, :invalid}
        account == nil -> claim(endpoint, wanted)
        true -> change(account, wanted)
      end
    end
  end

  defp write(_endpoint, _account, _body), do: {:error, :invalid}

  defp valid_body(body) do
    valid =
      Enum.all?(Map.keys(body), &(&1 in @account_keys)) and
        (not Map.has_key?(body, "name") or Names.valid_name?(body["name"])) and
        (not Map.has_key?(body, "kind") or body["kind"] in @kinds) and
        (not Map.has_key?(body, "searchable") or is_boolean(body["searchable"]))

    if valid, do: :ok, else: {:error, :invalid}
  end

  defp current(nil), do: %{"kind" => Protocol.default_kind(), "searchable" => false}

  defp current(%Account{} = account),
    do: %{"name" => account.name, "kind" => account.kind, "searchable" => account.searchable}

  defp claim(endpoint, %{"kind" => kind, "searchable" => searchable} = wanted) do
    cond do
      not Map.has_key?(wanted, "name") ->
        {:error, :invalid}

      # The type of an agent does not change, also after a deletion.
      Names.released_with_other_kind?(endpoint, kind) ->
        {:error, :invalid}

      true ->
        Repo.atomic(fn ->
          name = wanted["name"]

          account =
            Repo.insert!(%Account{
              endpoint: endpoint,
              name: name,
              number: number!(name, endpoint, Names.released_numbers(name, endpoint)),
              kind: kind,
              searchable: searchable,
              created_at: Clock.now(),
              version: 1
            })

          AccountCache.invalidate(endpoint)
          json(account)
        end)
    end
  end

  defp change(%Account{} = account, %{"name" => name, "kind" => kind, "searchable" => searchable}) do
    cond do
      kind != account.kind ->
        {:error, :invalid}

      name != account.name ->
        rename(account, name, searchable)

      true ->
        Repo.atomic(fn ->
          changed = account |> Ecto.Changeset.change(searchable: searchable) |> Repo.update!()
          AccountCache.invalidate(account.endpoint)
          json(changed)
        end)
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

      AccountCache.invalidate(endpoint)
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
    result =
      Versions.commit(fn ->
        touched = Enum.uniq(Invites.counterparts(endpoint) ++ Contacts.of(endpoint))
        Names.release(account.name, account.number, endpoint, account.kind)
        Invites.delete_for(endpoint)
        Contacts.delete_for(endpoint)
        end_named_rows(endpoint)
        Repo.delete!(account)
        AccountCache.invalidate(endpoint)
        {:ok, touched}
      end)

    AccountCache.invalidate(endpoint)
    result
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
      # A bad endpoint, or the node itself.
      _ -> {:error, :invalid}
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
