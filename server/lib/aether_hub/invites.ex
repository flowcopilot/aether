defmodule AetherHub.Invites do
  @moduledoc """
  Invites and access requests, with their states:

  - an invite is `pending`, the receiver makes it `accepted`, and the
    sender makes it `delivered` with the signed invite line;
  - a request (`origin` `request`) is `requested`, and the service agent
    makes it `accepted` (approve), then `delivered` as an invite.

  Every row expires a fixed time after it was made. A write bumps the
  versions of the sides that see the row and wakes their long polls.
  """

  import Ecto.Query

  alias AetherHub.{
    Account,
    Blocks,
    Clock,
    Contacts,
    Invite,
    Listings,
    Names,
    Notify,
    Params,
    Protocol,
    Repo,
    Versions
  }

  @ttl Protocol.limit(:invite_seconds)
  @days Protocol.limit(:access_days)
  @bundle_prefix Protocol.limit(:invite_prefix)
  @bundle_max Protocol.limit(:max_invite_bytes)
  @max_wait Protocol.limit(:max_wait_seconds)
  @max_open_requests Protocol.limit(:max_open_requests)
  @max_listed_requests Protocol.limit(:max_listed_requests)
  @personal Protocol.kind(:personal)
  @service Protocol.kind(:service)
  @origin_invite Protocol.invite_origin(:invite)
  @origin_request Protocol.invite_origin(:request)
  @status_pending Protocol.invite_status(:pending)
  @status_requested Protocol.invite_status(:requested)
  @status_accepted Protocol.invite_status(:accepted)
  @status_delivered Protocol.invite_status(:delivered)
  @role_sender Protocol.invite_role(:sender)
  @role_receiver Protocol.invite_role(:receiver)
  # The legal moves: {the side that makes it, the status before, the status after}.
  @transitions %{
    approve: {@role_sender, @status_requested, @status_accepted},
    accept: {@role_receiver, @status_pending, @status_accepted},
    deliver: {@role_sender, @status_accepted, @status_delivered}
  }
  @statuses_with_receiver_endpoint Enum.map(
                                     ~w(requested accepted delivered)a,
                                     &Protocol.invite_status/1
                                   )

  ## Making rows

  @doc "The personal agent `account` invites the personal agent `to` for `days` days."
  def invite(%Account{} = me, %{"to" => to, "days" => days})
      when is_binary(to) and is_integer(days) and days in @days do
    Versions.commit(fn ->
      # A service agent gets access only by request.
      if me.kind == @service, do: Repo.rollback(:forbidden)

      receiver =
        case Names.find(to) do
          {:ok, %Account{kind: @personal} = receiver} -> receiver
          # The same answer as for an unknown name: an invite does not
          # show that an unlisted service agent exists.
          {:ok, _service} -> Repo.rollback(:not_found)
          {:error, reason} -> Repo.rollback(reason)
        end

      if receiver.endpoint == me.endpoint, do: Repo.rollback(:invalid)
      insert(me, receiver, days, me.endpoint, @origin_invite)
    end)
  end

  def invite(_account, _body), do: {:error, :invalid}

  @doc """
  `account` asks the listed service agent `to` for access for `days` days.
  The row has the service as its sender, because the service signs the
  line. `{:ok, row}` for a new row, or `{:ok, row, :open}` with an open row
  between the two, unchanged.
  """
  def request(%Account{} = me, %{"to" => to, "days" => days} = body)
      when map_size(body) == 2 and is_binary(to) and is_integer(days) and days in @days do
    Versions.commit(fn ->
      service =
        case Names.find(to) do
          {:ok, %Account{kind: @service} = service} -> service
          {:ok, _personal} -> Repo.rollback(:not_found)
          {:error, reason} -> Repo.rollback(reason)
        end

      if service.endpoint == me.endpoint, do: Repo.rollback(:invalid)
      # An unlisted service agent gets the answer of an unknown name.
      unless Listings.listed?(service.endpoint), do: Repo.rollback(:not_found)

      case open_between(service.endpoint, me.endpoint) do
        %Invite{} = open ->
          {{json(open, Account.full_name(service), Account.full_name(me), me.endpoint), :open},
           []}

        nil ->
          if open_requests(me.endpoint) >= @max_open_requests, do: Repo.rollback(:limit)
          insert(service, me, days, me.endpoint, @origin_request)
      end
    end)
    |> case do
      {:ok, {row, :open}} -> {:ok, row, :open}
      result -> result
    end
  end

  def request(_account, _body), do: {:error, :invalid}

  defp open_between(one, other) do
    Repo.one(
      from i in open(),
        where:
          (i.sender_endpoint == ^one and i.receiver_endpoint == ^other) or
            (i.sender_endpoint == ^other and i.receiver_endpoint == ^one),
        limit: 1
    )
  end

  defp open_requests(requester) do
    Repo.aggregate(
      from(i in open(),
        where: i.receiver_endpoint == ^requester and i.origin == ^@origin_request
      ),
      :count
    )
  end

  # A new row from `sender` to `receiver`, seen as `asker`. It replaces an
  # expired or open row in the same direction. When the receiver of an
  # invite, or the service of a request, blocked the other side, the row
  # exists for that other side alone, and the answer has the same shape.
  defp insert(sender, receiver, days, asker, origin) do
    {status, target, maker} =
      case origin do
        @origin_invite -> {@status_pending, receiver.endpoint, sender.endpoint}
        @origin_request -> {@status_requested, sender.endpoint, receiver.endpoint}
      end

    Repo.delete_all(
      from i in Invite,
        where: i.sender_endpoint == ^sender.endpoint and i.receiver_endpoint == ^receiver.endpoint
    )

    invite =
      Repo.insert!(%Invite{
        id: Repo.new_id(),
        sender_endpoint: sender.endpoint,
        receiver_endpoint: receiver.endpoint,
        days: days,
        status: status,
        origin: origin,
        created_at: Clock.now(),
        hidden_from: Blocks.hidden_from(target, maker)
      })

    {json(invite, Account.full_name(sender), Account.full_name(receiver), asker), sides(invite)}
  end

  ## Moving rows

  @doc """
  The service agent approves the request `id`, for `days` days when the
  body names them.
  """
  def approve(%Account{} = account, id, body) do
    case approve_days(body) do
      {:ok, days} ->
        transition(account, id, :approve, if(days, do: [days: days], else: []))

      :error ->
        {:error, :invalid}
    end
  end

  defp approve_days(body) when body == %{}, do: {:ok, nil}

  defp approve_days(%{"days" => days} = body)
       when map_size(body) == 1 and is_integer(days) and days in @days,
       do: {:ok, days}

  defp approve_days(_body), do: :error

  def accept(%Account{} = account, id) do
    transition(account, id, :accept, [])
  end

  def deliver(%Account{} = account, id, %{"bundle" => bundle}) when is_binary(bundle) do
    if String.starts_with?(bundle, @bundle_prefix) and byte_size(bundle) <= @bundle_max do
      transition(account, id, :deliver, bundle: bundle)
    else
      {:error, :invalid}
    end
  end

  def deliver(_account, _id, _body), do: {:error, :invalid}

  # Makes the move `move` on the open row `id`, with the other `changes`.
  # The update checks the status again, so a row that moved since it was
  # read is forbidden too.
  defp transition(%Account{endpoint: endpoint}, id, move, changes) do
    {side, status, next} = Map.fetch!(@transitions, move)
    changes = Keyword.put(changes, :status, next)

    Versions.commit(fn ->
      invite = open!(endpoint, id)
      if role(invite, endpoint) != side or invite.status != status, do: Repo.rollback(:forbidden)

      case Repo.update_all(from(i in Invite, where: i.id == ^id and i.status == ^status),
             set: changes
           ) do
        {1, _} -> :ok
        {0, _} -> Repo.rollback(:forbidden)
      end

      invite = struct!(invite, changes)
      if invite.status == @status_delivered, do: Contacts.add(invite)
      {json(invite, endpoint), sides(invite)}
    end)
  end

  @doc "Either side removes the open row `id`."
  def delete(%Account{endpoint: endpoint}, id) do
    Versions.commit(fn ->
      invite = open!(endpoint, id)
      Repo.delete!(invite)
      {:ok, sides(invite)}
    end)
  end

  @doc """
  The endpoint that `account` blocks through the open row `id`: the sender
  of an invite that it received, or the node that asked it for access.
  Before delivery the receiver does not see the sender endpoint, so it
  names the row.
  """
  def block_target(%Account{endpoint: endpoint}, id) do
    case Repo.one(from i in open(), where: i.id == ^id) do
      %Invite{origin: @origin_invite, receiver_endpoint: ^endpoint, hidden_from: nil} = invite ->
        {:ok, invite.sender_endpoint}

      %Invite{origin: @origin_request, sender_endpoint: ^endpoint, hidden_from: nil} = invite ->
        {:ok, invite.receiver_endpoint}

      _ ->
        {:error, :not_found}
    end
  end

  ## Reading rows

  @doc """
  Lists the open rows of `account`. With `after` equal to the current
  version and `wait` above zero, waits up to `wait` seconds for a change.
  """
  def list(%Account{endpoint: endpoint}, params) do
    with {:ok, after_version, wait} <- parse_poll(params) do
      if is_integer(after_version) and wait > 0,
        do: wait_for_change(endpoint, after_version, wait)

      snapshot(endpoint)
    end
  end

  defp parse_poll(params) do
    with {:ok, after_version} <- Params.integer(params["after"], 0, nil),
         {:ok, wait} <- Params.integer(params["wait"], 0, @max_wait) do
      {:ok, after_version, wait || 0}
    else
      :error -> {:error, :invalid}
    end
  end

  defp wait_for_change(endpoint, after_version, wait) do
    # Subscribe before reading the version, so a write between the read and
    # the wait still wakes this request.
    Notify.subscribe(endpoint)

    try do
      if Versions.current(endpoint) == after_version, do: Notify.await(wait * 1000)
    after
      Notify.unsubscribe(endpoint)
    end
  end

  defp snapshot(endpoint) do
    Repo.atomic(fn ->
      invites =
        from(i in open(),
          join: s in Account,
          on: s.endpoint == i.sender_endpoint,
          join: r in Account,
          on: r.endpoint == i.receiver_endpoint,
          where: i.sender_endpoint == ^endpoint or i.receiver_endpoint == ^endpoint,
          where: ^Blocks.visible_to(endpoint),
          order_by: [asc: i.created_at, asc: i.id],
          select: {i, s, r}
        )
        |> Repo.all()
        |> limit_requests(endpoint)
        |> Enum.map(fn {i, from, to} ->
          json(i, Account.full_name(from), Account.full_name(to), endpoint)
        end)

      %{"version" => Versions.current(endpoint), "invites" => invites}
    end)
  end

  # A service agent sees the requests that wait for it from the oldest
  # requester accounts first: a new account cannot push older ones out of
  # the list.
  defp limit_requests(rows, endpoint) do
    {waiting, others} =
      Enum.split_with(rows, fn {i, _from, _to} ->
        i.status == @status_requested and i.sender_endpoint == endpoint
      end)

    waiting =
      waiting
      |> Enum.sort_by(fn {i, _from, to} -> {to.created_at, i.created_at, i.id} end)
      |> Enum.take(@max_listed_requests)

    Enum.sort_by(others ++ waiting, fn {i, _from, _to} -> {i.created_at, i.id} end)
  end

  ## Cascades and expiry

  @doc "The other sides of the open rows of `endpoint` that see them."
  def counterparts(endpoint) do
    from(i in open(),
      where: i.sender_endpoint == ^endpoint or i.receiver_endpoint == ^endpoint
    )
    |> Repo.all()
    |> Enum.flat_map(&sides/1)
    |> Enum.reject(&(&1 == endpoint))
  end

  def delete_for(endpoint) do
    Repo.delete_all(
      from i in Invite, where: i.sender_endpoint == ^endpoint or i.receiver_endpoint == ^endpoint
    )

    :ok
  end

  def delete_between(one, other) do
    Repo.delete_all(
      from i in Invite,
        where:
          (i.sender_endpoint == ^one and i.receiver_endpoint == ^other) or
            (i.sender_endpoint == ^other and i.receiver_endpoint == ^one)
    )

    :ok
  end

  @doc "Deletes every expired row and wakes both sides of each."
  def delete_expired do
    Versions.commit(fn ->
      expired = Repo.all(from i in Invite, where: i.created_at <= ^cutoff())
      Repo.delete_all(from i in Invite, where: i.id in ^Enum.map(expired, & &1.id))
      {:ok, Enum.flat_map(expired, &[&1.sender_endpoint, &1.receiver_endpoint])}
    end)
  end

  ## Helpers

  defp open!(endpoint, id) do
    invite = Repo.one(from i in open(), where: i.id == ^id)

    if invite == nil or endpoint not in [invite.sender_endpoint, invite.receiver_endpoint] or
         not Blocks.visible?(invite, endpoint) do
      Repo.rollback(:not_found)
    end

    invite
  end

  defp open, do: from(i in Invite, where: i.created_at > ^cutoff())

  defp cutoff, do: Clock.now() - @ttl

  defp sides(%Invite{} = invite),
    do:
      Enum.filter(
        [invite.sender_endpoint, invite.receiver_endpoint],
        &Blocks.visible?(invite, &1)
      )

  defp role(invite, endpoint),
    do: if(invite.sender_endpoint == endpoint, do: @role_sender, else: @role_receiver)

  defp json(invite, endpoint) do
    names =
      from(a in Account,
        where: a.endpoint in ^[invite.sender_endpoint, invite.receiver_endpoint]
      )
      |> Repo.all()
      |> Map.new(&{&1.endpoint, Account.full_name(&1)})

    json(invite, names[invite.sender_endpoint], names[invite.receiver_endpoint], endpoint)
  end

  defp json(invite, from, to, endpoint) do
    role = role(invite, endpoint)
    delivered = invite.status == @status_delivered

    %{
      "id" => invite.id,
      "from" => from,
      "to" => to,
      "role" => role,
      "days" => invite.days,
      "status" => invite.status,
      "origin" => invite.origin,
      "receiverEndpoint" =>
        if(role == @role_sender and invite.status in @statuses_with_receiver_endpoint,
          do: invite.receiver_endpoint
        ),
      "bundle" => if(role == @role_receiver and delivered, do: invite.bundle),
      # The receiver checks the line against the endpoint that sent it. It
      # does not learn that endpoint before the host acts.
      "senderEndpoint" => if(role == @role_receiver and delivered, do: invite.sender_endpoint),
      "createdAt" => invite.created_at
    }
  end
end
