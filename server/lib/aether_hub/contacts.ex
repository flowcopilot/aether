defmodule AetherHub.Contacts do
  @moduledoc """
  Contact pairs. The hub makes them when an invite is delivered. Only
  contacts see each other's presence and endpoint.
  """

  import Ecto.Query

  alias AetherHub.{Account, Contact, Invite, Presence, Protocol, Repo}

  @seen_step Protocol.limit(:last_seen_step_seconds)
  @origin_request Protocol.invite_origin(:request)

  @doc "The contacts of `account`, with their presence."
  def list(%Account{endpoint: endpoint}) do
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
          "handle" => Account.full_name(account),
          "kind" => account.kind,
          "endpoint" => contact,
          "online" => Presence.online?(contact),
          "lastSeen" => rounded_seen(Presence.last_seen(contact))
        }
      end)

    {:ok, %{"contacts" => contacts}}
  end

  # A contact sees the 15-minute period of the last request, not the second:
  # the exact time would show when the other person's computer is in use.
  defp rounded_seen(nil), do: nil
  defp rounded_seen(time), do: time - rem(time, @seen_step)

  @doc "Removes the contact pair between `account` and `contact`."
  def delete(%Account{endpoint: endpoint}, contact) do
    with :ok <- Protocol.validate_endpoint(contact) do
      case delete_between(endpoint, contact) do
        0 -> {:error, :not_found}
        _ -> :ok
      end
    end
  end

  @doc "The endpoints in `endpoints` that are contacts of `asker`."
  def known(asker, endpoints) do
    from(c in Contact,
      where: c.owner_endpoint == ^asker and c.contact_endpoint in ^endpoints,
      select: c.contact_endpoint
    )
    |> Repo.all()
    |> MapSet.new()
  end

  def of(endpoint) do
    Repo.all(from c in Contact, where: c.owner_endpoint == ^endpoint, select: c.contact_endpoint)
  end

  @doc """
  Adds the contacts of a delivered invite. After a request, the requester
  sees the service agent online; the service does not see the requester.
  """
  def add(%Invite{sender_endpoint: sender, receiver_endpoint: receiver, origin: origin}) do
    pairs =
      case origin do
        @origin_request ->
          [%{owner_endpoint: receiver, contact_endpoint: sender}]

        _ ->
          [
            %{owner_endpoint: sender, contact_endpoint: receiver},
            %{owner_endpoint: receiver, contact_endpoint: sender}
          ]
      end

    Repo.insert_all(Contact, pairs,
      on_conflict: :nothing,
      conflict_target: [:owner_endpoint, :contact_endpoint]
    )

    :ok
  end

  @doc "Removes the pairs between `one` and `other`. Returns the count."
  def delete_between(one, other) do
    {count, _} =
      Repo.delete_all(
        from c in Contact,
          where:
            (c.owner_endpoint == ^one and c.contact_endpoint == ^other) or
              (c.owner_endpoint == ^other and c.contact_endpoint == ^one)
      )

    count
  end

  def delete_for(endpoint) do
    Repo.delete_all(
      from c in Contact, where: c.owner_endpoint == ^endpoint or c.contact_endpoint == ^endpoint
    )

    :ok
  end
end
