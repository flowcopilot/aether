defmodule AetherHub.Hub do
  @moduledoc """
  The hub's rules: names, invites, and the per-endpoint version counter.

  Every function takes the verified endpoint of the caller. Errors are atoms
  that match the protocol error codes.
  """

  import Ecto.Query

  alias AetherHub.{Account, Blocks, Contact, Invite, Listings, Names, Notify, Presence, Repo}

  @ttl 7 * 24 * 60 * 60
  @bundle_prefix "aether-invite:"
  @bundle_max 8 * 1024
  @max_wait 30

  ## Accounts

  def get_account(endpoint) do
    case Repo.get(Account, endpoint) do
      nil -> {:error, :not_found}
      account -> {:ok, account_json(account)}
    end
  end

  @account_keys ["name", "kind", "searchable"]
  @kinds ["personal", "service"]

  @doc """
  Claims an account for `endpoint`, or changes its name or its search
  setting. A first claim counts against the new accounts of `client_ip`. A
  rename counts against the renames of the endpoint and keeps the current
  number when it is free for the new name.
  """
  def put_account(endpoint, body, client_ip \\ {127, 0, 0, 1})

  def put_account(endpoint, %{} = body, client_ip) do
    with :ok <- valid_account_body(body) do
      case Repo.get(Account, endpoint) do
        nil -> first_claim(endpoint, body, client_ip)
        account -> change_account(account, body)
      end
    end
  end

  def put_account(_endpoint, _body, _client_ip), do: {:error, :invalid}

  defp valid_account_body(body) do
    valid =
      Enum.all?(Map.keys(body), &(&1 in @account_keys)) and
        (not Map.has_key?(body, "name") or Names.valid_name?(body["name"])) and
        (not Map.has_key?(body, "kind") or body["kind"] in @kinds) and
        (not Map.has_key?(body, "searchable") or is_boolean(body["searchable"]))

    if valid, do: :ok, else: {:error, :invalid}
  end

  defp first_claim(endpoint, body, client_ip) do
    kind = Map.get(body, "kind", "personal")
    searchable = Map.get(body, "searchable", false)

    cond do
      not Map.has_key?(body, "name") ->
        {:error, :invalid}

      kind == "service" and searchable ->
        {:error, :invalid}

      # The type of an agent does not change, also after a deletion.
      Names.released_with_other_kind?(endpoint, kind) ->
        {:error, :invalid}

      AetherHub.RateLimit.hit(:new_account_ip, client_ip) == :rate_limited ->
        {:error, :rate_limited}

      true ->
        insert_account(endpoint, body["name"], kind, searchable)
    end
  end

  defp insert_account(endpoint, name, kind, searchable) do
    result =
      transact(fn ->
        number = number!(name, endpoint, Names.released_numbers(name, endpoint))

        Repo.insert!(%Account{
          endpoint: endpoint,
          name: name,
          number: number,
          kind: kind,
          searchable: searchable,
          created_at: now(),
          version: 1
        })
      end)

    with {:ok, account} <- result, do: {:ok, account_json(account)}
  end

  defp change_account(%Account{} = account, body) do
    name = Map.get(body, "name", account.name)
    searchable = Map.get(body, "searchable", account.searchable)

    cond do
      Map.get(body, "kind", account.kind) != account.kind -> {:error, :invalid}
      account.kind == "service" and searchable -> {:error, :invalid}
      name != account.name -> rename(account, name, searchable)
      true -> update_account(account, searchable: searchable)
    end
  end

  defp update_account(account, changes) do
    {:ok, account |> Ecto.Changeset.change(changes) |> Repo.update!() |> account_json()}
  end

  defp rename(%Account{endpoint: endpoint} = account, name, searchable) do
    if AetherHub.RateLimit.hit(:rename, endpoint) == :rate_limited do
      {:error, :rate_limited}
    else
      result =
        transact(fn ->
          preferred = [account.number | Names.released_numbers(name, endpoint)]
          number = number!(name, endpoint, preferred)
          Names.release(account.name, account.number, endpoint, account.kind)
          # A listing and an owner link name the agent, so they end with
          # the old name.
          Listings.delete(endpoint)
          AetherHub.OwnerLinks.delete_for(endpoint)

          renamed =
            account
            |> Ecto.Changeset.change(name: name, number: number, searchable: searchable)
            |> Repo.update!()

          touched = [endpoint | counterparts(endpoint)]
          bump(touched)
          {renamed, touched}
        end)

      with {:ok, {renamed, touched}} <- result do
        Notify.changed(touched)
        {:ok, account_json(renamed)}
      end
    end
  end

  defp number!(name, endpoint, preferred) do
    case Names.pick_number(name, endpoint, preferred) do
      {:ok, number} -> number
      {:error, :taken} -> Repo.rollback(:taken)
    end
  end

  defp find_account(handle) do
    with {:ok, {name, number}} <- Names.parse_handle(handle) do
      case Repo.one(from a in Account, where: a.name == ^name and a.number == ^number) do
        nil -> {:error, :not_found}
        account -> {:ok, account}
      end
    else
      :error -> {:error, :invalid}
    end
  end

  # The other sides of the open invites of `endpoint` that can see them.
  defp counterparts(endpoint) do
    from(i in open_invites(),
      where: i.sender_endpoint == ^endpoint or i.receiver_endpoint == ^endpoint,
      select: i
    )
    |> Repo.all()
    |> Enum.flat_map(&visible_sides/1)
    |> Enum.reject(&(&1 == endpoint))
  end

  # The sides that see `invite`: both, or the sender alone when the
  # receiver blocked it.
  defp visible_sides(%Invite{} = invite),
    do:
      Enum.reject([invite.sender_endpoint, invite.receiver_endpoint], &(&1 == invite.hidden_from))

  @doc """
  Deletes the account of `endpoint`, its invites and its contacts. Its tag
  stays with the key, with its type; blocks stay too.
  """
  def delete_account(endpoint) do
    result =
      transact(fn ->
        account = account!(endpoint)
        contacts = contact_endpoints(endpoint)
        touched = Enum.uniq(counterparts(endpoint) ++ contacts)
        Names.release(account.name, account.number, endpoint, account.kind)

        Repo.delete_all(
          from i in Invite,
            where: i.sender_endpoint == ^endpoint or i.receiver_endpoint == ^endpoint
        )

        Repo.delete_all(
          from c in Contact,
            where: c.owner_endpoint == ^endpoint or c.contact_endpoint == ^endpoint
        )

        Listings.delete(endpoint)
        AetherHub.OwnerLinks.delete_for(endpoint)
        Repo.delete!(account)
        bump(touched)
        {:ok, touched}
      end)

    with {:ok, :ok} <- notify(result), do: :ok
  end

  defp contact_endpoints(endpoint) do
    Repo.all(from c in Contact, where: c.owner_endpoint == ^endpoint, select: c.contact_endpoint)
  end

  ## Listings

  @doc "Stops the listing of `endpoint`. `:ok` also when it has none."
  def delete_listing(endpoint) do
    with {:ok, _} <- fetch_account(endpoint), do: Listings.delete(endpoint)
  end

  ## Blocks

  @endpoint_format ~r/\A[0-9a-f]{64}\z/

  @doc """
  `endpoint` blocks `blocked`: the open invites between the two and their
  contact pair go, and new invites from `blocked` exist for it alone.
  """
  def block(endpoint, blocked) do
    if Regex.match?(@endpoint_format, blocked) and blocked != endpoint do
      result =
        transact(fn ->
          account!(endpoint)

          case Blocks.put(endpoint, blocked) do
            :ok -> :ok
            {:error, reason} -> Repo.rollback(reason)
          end

          Repo.delete_all(
            from i in Invite,
              where:
                (i.sender_endpoint == ^endpoint and i.receiver_endpoint == ^blocked) or
                  (i.sender_endpoint == ^blocked and i.receiver_endpoint == ^endpoint)
          )

          Repo.delete_all(
            from c in Contact,
              where:
                (c.owner_endpoint == ^endpoint and c.contact_endpoint == ^blocked) or
                  (c.owner_endpoint == ^blocked and c.contact_endpoint == ^endpoint)
          )

          AetherHub.OwnerLinks.delete_between(endpoint, blocked)
          touched = [endpoint, blocked]
          bump(touched)
          {:ok, touched}
        end)

      with {:ok, :ok} <- notify(result), do: :ok
    else
      {:error, :invalid}
    end
  end

  @doc """
  The receiver of the open invite `id` blocks its sender, or the service
  agent of the open request `id` blocks the node that asked. Before
  delivery the receiver does not see the sender endpoint, so it names the
  invite.
  """
  def block_invite(endpoint, id) do
    with {:ok, _} <- fetch_account(endpoint) do
      case Repo.one(from i in open_invites(), where: i.id == ^id) do
        %Invite{origin: "invite", receiver_endpoint: ^endpoint, hidden_from: nil} = invite ->
          block(endpoint, invite.sender_endpoint)

        # The service agent blocks the node that asked it for access.
        %Invite{origin: "request", sender_endpoint: ^endpoint, hidden_from: nil} = invite ->
          block(endpoint, invite.receiver_endpoint)

        _ ->
          {:error, :not_found}
      end
    end
  end

  @doc "Removes the block of `blocked` by `endpoint`."
  def unblock(endpoint, blocked) do
    with {:ok, _} <- fetch_account(endpoint), do: Blocks.delete(endpoint, blocked)
  end

  @doc "The blocks of `endpoint`."
  def list_blocks(endpoint) do
    with {:ok, _} <- fetch_account(endpoint), do: {:ok, %{"blocks" => Blocks.list(endpoint)}}
  end

  ## Invites

  def create_invite(endpoint, %{"to" => to, "days" => days})
      when is_binary(to) and is_integer(days) and days in 1..30 do
    result =
      transact(fn ->
        me = account!(endpoint)

        # A service agent gets access only by request.
        if me.kind == "service", do: Repo.rollback(:forbidden)

        receiver =
          case find_account(to) do
            {:ok, %Account{kind: "personal"} = receiver} -> receiver
            # The same answer as for an unknown name: an invite does not
            # show that an unlisted service agent exists.
            {:ok, _service} -> Repo.rollback(:not_found)
            {:error, reason} -> Repo.rollback(reason)
          end

        if receiver.endpoint == me.endpoint, do: Repo.rollback(:invalid)

        # The receiver blocked the sender: the row exists for the sender
        # alone, and the answer has the same shape.
        hidden_from =
          if Blocks.blocked?(receiver.endpoint, me.endpoint), do: receiver.endpoint

        Repo.delete_all(
          from i in Invite,
            where: i.sender_endpoint == ^me.endpoint and i.receiver_endpoint == ^receiver.endpoint
        )

        invite =
          Repo.insert!(%Invite{
            id: Base.encode16(:crypto.strong_rand_bytes(16), case: :lower),
            sender_endpoint: me.endpoint,
            receiver_endpoint: receiver.endpoint,
            days: days,
            status: "pending",
            created_at: now(),
            hidden_from: hidden_from
          })

        touched = visible_sides(invite)
        bump(touched)
        {invite_json(invite, full_name(me), full_name(receiver), endpoint), touched}
      end)

    notify(result)
  end

  def create_invite(_endpoint, _body), do: {:error, :invalid}

  @max_open_requests 5

  @doc """
  `endpoint` asks the listed service agent `to` for access for `days` days.
  The row has the service as its sender, because the service signs the
  line. An open row between the two comes back without a change.
  """
  def create_request(endpoint, %{"to" => to, "days" => days} = body)
      when map_size(body) == 2 and is_binary(to) and is_integer(days) and days in 1..30 do
    result =
      transact(fn ->
        me = account!(endpoint)

        service =
          case find_account(to) do
            {:ok, %Account{kind: "service"} = service} -> service
            {:ok, _personal} -> Repo.rollback(:not_found)
            {:error, reason} -> Repo.rollback(reason)
          end

        if service.endpoint == me.endpoint, do: Repo.rollback(:invalid)
        # An unlisted service agent gets the answer of an unknown name.
        unless Listings.listed?(service.endpoint, now()), do: Repo.rollback(:not_found)

        case Repo.one(
               from i in open_invites(),
                 where:
                   (i.sender_endpoint == ^service.endpoint and i.receiver_endpoint == ^me.endpoint) or
                     (i.sender_endpoint == ^me.endpoint and
                        i.receiver_endpoint == ^service.endpoint),
                 limit: 1
             ) do
          %Invite{} = open ->
            {invite_json(open, full_name(service), full_name(me), endpoint), []}

          nil ->
            insert_request(me, service, days)
        end
      end)

    notify(result)
  end

  def create_request(_endpoint, _body), do: {:error, :invalid}

  defp insert_request(me, service, days) do
    open =
      Repo.aggregate(
        from(i in open_invites(),
          where: i.receiver_endpoint == ^me.endpoint and i.origin == "request"
        ),
        :count
      )

    if open >= @max_open_requests, do: Repo.rollback(:limit)

    if AetherHub.RateLimit.hit(:access_request, me.endpoint) == :rate_limited,
      do: Repo.rollback(:rate_limited)

    # The service blocked the requester: the row exists for the requester
    # alone, and the answer has the same shape.
    hidden_from =
      if Blocks.blocked?(service.endpoint, me.endpoint), do: service.endpoint

    # No row between the two is open, so one that is left has expired.
    Repo.delete_all(
      from i in Invite,
        where: i.sender_endpoint == ^service.endpoint and i.receiver_endpoint == ^me.endpoint
    )

    invite =
      Repo.insert!(%Invite{
        id: Base.encode16(:crypto.strong_rand_bytes(16), case: :lower),
        sender_endpoint: service.endpoint,
        receiver_endpoint: me.endpoint,
        days: days,
        status: "requested",
        origin: "request",
        created_at: now(),
        hidden_from: hidden_from
      })

    touched = visible_sides(invite)
    bump(touched)
    {invite_json(invite, full_name(service), full_name(me), me.endpoint), touched}
  end

  @doc """
  The service agent approves the request `id`, for `days` days when the
  body names them. The requester then sees `accepted`, and the service
  delivers the line as for an invite.
  """
  def approve_request(endpoint, id, body) do
    case approve_days(body) do
      {:ok, days} ->
        transition(endpoint, id, fn invite, role ->
          if role != "sender" or invite.status != "requested", do: Repo.rollback(:forbidden)
          Ecto.Changeset.change(invite, status: "accepted", days: days || invite.days)
        end)

      :error ->
        {:error, :invalid}
    end
  end

  defp approve_days(body) when body == %{}, do: {:ok, nil}

  defp approve_days(%{"days" => days} = body)
       when map_size(body) == 1 and is_integer(days) and days in 1..30,
       do: {:ok, days}

  defp approve_days(_body), do: :error

  @doc """
  Lists the open invites of `endpoint`. With `after` equal to the current
  version and `wait` above zero, waits up to `wait` seconds for a change.
  """
  def list_invites(endpoint, params) do
    with {:ok, after_version, wait} <- parse_poll(params),
         {:ok, _} <- fetch_account(endpoint) do
      if is_integer(after_version) and wait > 0,
        do: wait_for_change(endpoint, after_version, wait)

      snapshot(endpoint)
    end
  end

  defp wait_for_change(endpoint, after_version, wait) do
    # Subscribe before reading the version, so a write between the read and
    # the wait still wakes this request.
    Notify.subscribe(endpoint)

    try do
      if current_version(endpoint) == after_version, do: Notify.await(wait * 1000)
    after
      Notify.unsubscribe(endpoint)
    end
  end

  defp snapshot(endpoint) do
    transact(fn ->
      version = current_version(endpoint)

      invites =
        from(i in open_invites(),
          join: s in Account,
          on: s.endpoint == i.sender_endpoint,
          join: r in Account,
          on: r.endpoint == i.receiver_endpoint,
          where: i.sender_endpoint == ^endpoint or i.receiver_endpoint == ^endpoint,
          where: is_nil(i.hidden_from) or i.hidden_from != ^endpoint,
          order_by: [asc: i.created_at, asc: i.id],
          select: {i, s, r}
        )
        |> Repo.all()
        |> limit_requests(endpoint)
        |> Enum.map(fn {i, from, to} ->
          invite_json(i, full_name(from), full_name(to), endpoint)
        end)

      %{"version" => version, "invites" => invites}
    end)
  end

  @max_listed_requests 50

  # A service agent sees at most 50 requests that wait for it, from the
  # oldest requester accounts first: a new account cannot push older ones
  # out of the list.
  defp limit_requests(rows, endpoint) do
    {waiting, others} =
      Enum.split_with(rows, fn {i, _from, _to} ->
        i.status == "requested" and i.sender_endpoint == endpoint
      end)

    waiting =
      waiting
      |> Enum.sort_by(fn {i, _from, to} -> {to.created_at, i.created_at, i.id} end)
      |> Enum.take(@max_listed_requests)

    Enum.sort_by(others ++ waiting, fn {i, _from, _to} -> {i.created_at, i.id} end)
  end

  def accept_invite(endpoint, id) do
    transition(endpoint, id, fn invite, role ->
      if role != "receiver" or invite.status != "pending", do: Repo.rollback(:forbidden)
      Ecto.Changeset.change(invite, status: "accepted")
    end)
  end

  def deliver_bundle(endpoint, id, %{"bundle" => bundle}) when is_binary(bundle) do
    if String.starts_with?(bundle, @bundle_prefix) and byte_size(bundle) <= @bundle_max do
      transition(endpoint, id, fn invite, role ->
        if role != "sender" or invite.status != "accepted", do: Repo.rollback(:forbidden)
        Ecto.Changeset.change(invite, status: "delivered", bundle: bundle)
      end)
    else
      {:error, :invalid}
    end
  end

  def deliver_bundle(_endpoint, _id, _body), do: {:error, :invalid}

  def delete_invite(endpoint, id) do
    result =
      transact(fn ->
        account!(endpoint)
        invite = open_invite!(endpoint, id)
        Repo.delete!(invite)
        touched = visible_sides(invite)
        bump(touched)
        {:ok, touched}
      end)

    with {:ok, :ok} <- notify(result), do: :ok
  end

  @doc "Deletes every expired invite and wakes both sides of each."
  def delete_expired do
    {:ok, touched} =
      transact(fn ->
        expired =
          Repo.all(from i in Invite, where: i.created_at <= ^cutoff())

        Repo.delete_all(from i in Invite, where: i.id in ^Enum.map(expired, & &1.id))
        touched = Enum.flat_map(expired, &[&1.sender_endpoint, &1.receiver_endpoint])
        bump(touched)
        touched
      end)

    Notify.changed(touched)
  end

  defp transition(endpoint, id, change) do
    result =
      transact(fn ->
        account!(endpoint)
        invite = open_invite!(endpoint, id)
        role = role(invite, endpoint)
        invite = invite |> change.(role) |> Repo.update!()
        if invite.status == "delivered", do: add_contacts(invite)
        touched = visible_sides(invite)
        bump(touched)
        {from, to} = handles(invite)
        {invite_json(invite, from, to, endpoint), touched}
      end)

    notify(result)
  end

  ## Contacts

  @doc "Lists the contacts of `endpoint` with their presence at Unix time `now`."
  def list_contacts(endpoint, now \\ now()) do
    with {:ok, _} <- fetch_account(endpoint) do
      contacts =
        from(c in Contact,
          join: a in Account,
          on: a.endpoint == c.contact_endpoint,
          where: c.owner_endpoint == ^endpoint,
          order_by: [asc: a.name, asc: a.number],
          select: a
        )
        |> Repo.all()
        |> Enum.map(fn %Account{endpoint: contact} = account ->
          %{
            "handle" => full_name(account),
            "kind" => account.kind,
            "endpoint" => contact,
            "online" => Presence.online?(contact, now),
            "lastSeen" => rounded_seen(Presence.last_seen(contact))
          }
        end)

      {:ok, %{"contacts" => contacts}}
    end
  end

  # A contact sees the 15-minute period of the last request, not the second:
  # the exact time would show when the other person's computer is in use.
  @seen_step 900

  defp rounded_seen(nil), do: nil
  defp rounded_seen(time), do: time - rem(time, @seen_step)

  @doc "Removes the contact pair between `endpoint` and `contact`, in both directions."
  def delete_contact(endpoint, contact) do
    with {:ok, _} <- fetch_account(endpoint) do
      {count, _} =
        Repo.delete_all(
          from c in Contact,
            where:
              (c.owner_endpoint == ^endpoint and c.contact_endpoint == ^contact) or
                (c.owner_endpoint == ^contact and c.contact_endpoint == ^endpoint)
        )

      if count > 0, do: :ok, else: {:error, :not_found}
    end
  end

  defp add_contacts(%Invite{sender_endpoint: sender, receiver_endpoint: receiver} = invite) do
    # After a request, the requester sees the service agent online; the
    # service does not see the requester.
    pairs =
      case invite.origin do
        "request" ->
          [%{owner_endpoint: receiver, contact_endpoint: sender}]

        _ ->
          [
            %{owner_endpoint: sender, contact_endpoint: receiver},
            %{owner_endpoint: receiver, contact_endpoint: sender}
          ]
      end

    Repo.insert_all(
      Contact,
      pairs,
      on_conflict: :nothing,
      conflict_target: [:owner_endpoint, :contact_endpoint]
    )
  end

  ## Helpers

  defp open_invite!(endpoint, id) do
    invite = Repo.one(from i in open_invites(), where: i.id == ^id)

    if invite == nil or endpoint not in [invite.sender_endpoint, invite.receiver_endpoint] or
         invite.hidden_from == endpoint do
      Repo.rollback(:not_found)
    end

    invite
  end

  defp open_invites, do: from(i in Invite, where: i.created_at > ^cutoff())

  defp handles(invite) do
    accounts =
      from(a in Account,
        where: a.endpoint in ^[invite.sender_endpoint, invite.receiver_endpoint],
        select: a
      )
      |> Repo.all()
      |> Map.new(&{&1.endpoint, full_name(&1)})

    {accounts[invite.sender_endpoint], accounts[invite.receiver_endpoint]}
  end

  defp role(invite, endpoint),
    do: if(invite.sender_endpoint == endpoint, do: "sender", else: "receiver")

  defp invite_json(invite, from, to, endpoint) do
    role = role(invite, endpoint)

    %{
      "id" => invite.id,
      "from" => from,
      "to" => to,
      "role" => role,
      "days" => invite.days,
      "status" => invite.status,
      "origin" => invite.origin,
      "receiverEndpoint" =>
        if(role == "sender" and invite.status in ["requested", "accepted", "delivered"],
          do: invite.receiver_endpoint
        ),
      "bundle" => if(role == "receiver" and invite.status == "delivered", do: invite.bundle),
      # The receiver checks the line against the endpoint that sent it. It
      # does not learn that endpoint before the host acts.
      "senderEndpoint" =>
        if(role == "receiver" and invite.status == "delivered", do: invite.sender_endpoint),
      "createdAt" => invite.created_at
    }
  end

  defp full_name(%Account{name: name, number: number}), do: Names.format(name, number)

  defp account_json(%Account{} = account) do
    %{
      "handle" => full_name(account),
      "name" => account.name,
      "number" => account.number,
      "endpoint" => account.endpoint,
      "kind" => account.kind,
      "searchable" => account.searchable
    }
  end

  defp fetch_account(endpoint) do
    case Repo.get(Account, endpoint) do
      nil -> {:error, :no_account}
      account -> {:ok, account}
    end
  end

  defp account!(endpoint) do
    case fetch_account(endpoint) do
      {:ok, account} -> account
      {:error, reason} -> Repo.rollback(reason)
    end
  end

  defp current_version(endpoint) do
    Repo.one(from a in Account, where: a.endpoint == ^endpoint, select: a.version)
  end

  defp bump([]), do: :ok

  defp bump(endpoints) do
    Repo.update_all(from(a in Account, where: a.endpoint in ^Enum.uniq(endpoints)),
      inc: [version: 1]
    )

    :ok
  end

  defp notify({:ok, {value, touched}}) do
    Notify.changed(touched)
    {:ok, value}
  end

  defp notify({:error, _} = error), do: error

  defp parse_poll(params) do
    with {:ok, after_version} <- optional_int(params["after"], 0, nil),
         {:ok, wait} <- optional_int(params["wait"], 0, @max_wait) do
      {:ok, after_version, wait || 0}
    end
  end

  defp optional_int(nil, _min, _max), do: {:ok, nil}

  defp optional_int(value, min, max) when is_binary(value) do
    case Integer.parse(value) do
      {n, ""} when n >= min and (max == nil or n <= max) -> {:ok, n}
      _ -> {:error, :invalid}
    end
  end

  defp optional_int(_value, _min, _max), do: {:error, :invalid}

  defp transact(fun), do: Repo.transact(fn -> {:ok, fun.()} end)

  defp now, do: System.system_time(:second)
  defp cutoff, do: now() - @ttl
end
