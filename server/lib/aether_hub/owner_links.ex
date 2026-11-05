defmodule AetherHub.OwnerLinks do
  @moduledoc """
  Owner links: a service agent linked to the personal agent of its owner.
  The personal node signs a claim for the full name of the service, and the
  service node signs a confirm for the personal endpoint. Then the listing
  of the service shows the full name of the owner. A service has at most one
  owner.

  Both signatures are kept, so a node can check the link again.
  """

  import Ecto.Query

  alias AetherHub.{Account, Blocks, Names, OwnerLink, Repo, Signature}

  @max_days 365
  @claim_days 7
  @max_open_claims 5
  @max_links 20

  @doc "The message that the personal node signs for its claim."
  def claim_message(personal_endpoint, service_handle, expires_at) do
    Enum.join(
      [
        "aether-owner/v1",
        "claim",
        personal_endpoint,
        service_handle,
        Integer.to_string(expires_at)
      ],
      "\n"
    )
  end

  @doc "The message that the service node signs for its confirm."
  def confirm_message(service_endpoint, personal_endpoint, expires_at) do
    Enum.join(
      [
        "aether-owner/v1",
        "confirm",
        service_endpoint,
        personal_endpoint,
        Integer.to_string(expires_at)
      ],
      "\n"
    )
  end

  @doc """
  The personal agent `endpoint` claims the service agent in `body` at Unix
  time `now`. A new claim for the same service replaces the old link, which
  the service must confirm again.
  """
  def claim(
        endpoint,
        %{"service" => service, "expiresAt" => expires_at, "signature" => signature} = body,
        now
      )
      when map_size(body) == 3 and is_binary(service) and is_integer(expires_at) and
             is_binary(signature) do
    Repo.transact(fn ->
      with {:ok, %Account{kind: "personal"} = me} <- fetch_account(endpoint),
           {:ok, %Account{kind: "service"} = target} <- find_service(service),
           true <- expires_at > now and expires_at <= now + @max_days * 86_400,
           true <- signed?(endpoint, claim_message(endpoint, service, expires_at), signature),
           :ok <- within_limits(me.endpoint, target.endpoint, now) do
        Repo.delete_all(
          from l in OwnerLink,
            where: l.service_endpoint == ^target.endpoint and l.personal_endpoint == ^me.endpoint
        )

        hidden_from = if Blocks.blocked?(target.endpoint, me.endpoint), do: target.endpoint

        link =
          Repo.insert!(%OwnerLink{
            id: Base.encode16(:crypto.strong_rand_bytes(16), case: :lower),
            service_endpoint: target.endpoint,
            personal_endpoint: me.endpoint,
            service_handle: service,
            expires_at: expires_at,
            claim_signature: signature,
            hidden_from: hidden_from,
            created_at: now
          })

        {:ok, link_json(link, full_name(target), full_name(me), endpoint, now)}
      else
        {:ok, %Account{kind: "service"}} -> {:error, :forbidden}
        {:ok, _personal_target} -> {:error, :not_found}
        {:error, reason} -> {:error, reason}
        false -> {:error, :invalid}
      end
    end)
  end

  def claim(_endpoint, _body, _now), do: {:error, :invalid}

  defp find_service(handle) do
    case Names.parse_handle(handle) do
      {:ok, {name, number}} ->
        case Repo.one(from a in Account, where: a.name == ^name and a.number == ^number) do
          nil -> {:error, :not_found}
          account -> {:ok, account}
        end

      :error ->
        {:error, :invalid}
    end
  end

  defp within_limits(personal, service, now) do
    others =
      from(l in OwnerLink,
        where: l.personal_endpoint == ^personal and l.service_endpoint != ^service
      )

    open =
      Repo.aggregate(
        from(l in others,
          where: is_nil(l.confirm_signature) and l.created_at > ^(now - @claim_days * 86_400)
        ),
        :count
      )

    links = Repo.aggregate(others, :count)

    if open >= @max_open_claims or links >= @max_links, do: {:error, :limit}, else: :ok
  end

  @doc """
  The service agent `endpoint` confirms the claim `id` with the signature in
  `body` at Unix time `now`.
  """
  def confirm(endpoint, id, %{"signature" => signature} = body, now)
      when map_size(body) == 1 and is_binary(signature) do
    Repo.transact(fn ->
      with {:ok, service} <- fetch_account(endpoint),
           %OwnerLink{} = link <- visible(endpoint, id, now),
           :ok <- confirmable(link, endpoint),
           true <-
             signed?(
               endpoint,
               confirm_message(endpoint, link.personal_endpoint, link.expires_at),
               signature
             ),
           :ok <- no_other_owner(endpoint, link.id, now) do
        link = link |> Ecto.Changeset.change(confirm_signature: signature) |> Repo.update!()
        personal = Repo.get!(Account, link.personal_endpoint)
        {:ok, link_json(link, full_name(service), full_name(personal), endpoint, now)}
      else
        nil -> {:error, :not_found}
        false -> {:error, :invalid}
        {:error, reason} -> {:error, reason}
      end
    end)
  end

  def confirm(_endpoint, _id, _body, _now), do: {:error, :invalid}

  defp confirmable(%OwnerLink{service_endpoint: endpoint, confirm_signature: nil}, endpoint),
    do: :ok

  defp confirmable(_link, _endpoint), do: {:error, :forbidden}

  defp no_other_owner(service, id, now) do
    if Repo.exists?(
         from l in OwnerLink,
           where:
             l.service_endpoint == ^service and l.id != ^id and not is_nil(l.confirm_signature) and
               l.expires_at > ^now
       ),
       do: {:error, :limit},
       else: :ok
  end

  @doc "The links and claims that `endpoint` can see at Unix time `now`."
  def list(endpoint, now) do
    with {:ok, _} <- fetch_account(endpoint) do
      links =
        from(l in live(now),
          join: s in Account,
          on: s.endpoint == l.service_endpoint,
          join: p in Account,
          on: p.endpoint == l.personal_endpoint,
          where:
            l.personal_endpoint == ^endpoint or
              (l.service_endpoint == ^endpoint and
                 (is_nil(l.hidden_from) or l.hidden_from != ^endpoint)),
          order_by: [asc: l.created_at, asc: l.id],
          select: {l, s, p}
        )
        |> Repo.all()
        |> Enum.map(fn {link, service, personal} ->
          link_json(link, full_name(service), full_name(personal), endpoint, now)
        end)

      {:ok, %{"links" => links}}
    end
  end

  @doc "Either side removes the link `id`."
  def delete(endpoint, id) do
    with {:ok, _} <- fetch_account(endpoint) do
      {count, _} =
        Repo.delete_all(
          from l in OwnerLink,
            where:
              l.id == ^id and
                (l.personal_endpoint == ^endpoint or
                   (l.service_endpoint == ^endpoint and
                      (is_nil(l.hidden_from) or l.hidden_from != ^endpoint)))
        )

      if count > 0, do: :ok, else: {:error, :not_found}
    end
  end

  @doc "Removes every link of `endpoint`, on either side."
  def delete_for(endpoint) do
    Repo.delete_all(
      from l in OwnerLink,
        where: l.service_endpoint == ^endpoint or l.personal_endpoint == ^endpoint
    )

    :ok
  end

  @doc "Removes the links between `one` and `other`, in either direction."
  def delete_between(one, other) do
    Repo.delete_all(
      from l in OwnerLink,
        where:
          (l.service_endpoint == ^one and l.personal_endpoint == ^other) or
            (l.service_endpoint == ^other and l.personal_endpoint == ^one)
    )

    :ok
  end

  @doc """
  The full name of the owner of the service agent `service` at Unix time
  `now`, or `nil`.
  """
  def active_owner(service, now) do
    from(l in OwnerLink,
      join: p in Account,
      on: p.endpoint == l.personal_endpoint,
      where:
        l.service_endpoint == ^service and not is_nil(l.confirm_signature) and
          l.expires_at > ^now,
      select: {p.name, p.number},
      limit: 1
    )
    |> Repo.one()
    |> case do
      nil -> nil
      {name, number} -> Names.format(name, number)
    end
  end

  @doc """
  Removes the links that expired at Unix time `now`, and the claims that
  waited 7 days.
  """
  def delete_expired(now \\ System.system_time(:second)) do
    Repo.delete_all(
      from l in OwnerLink,
        where:
          l.expires_at <= ^now or
            (is_nil(l.confirm_signature) and l.created_at <= ^(now - @claim_days * 86_400))
    )

    :ok
  end

  # The links that count at `now`: active ones, and claims of the last 7
  # days.
  defp live(now) do
    from l in OwnerLink,
      where:
        l.expires_at > ^now and
          (not is_nil(l.confirm_signature) or l.created_at > ^(now - @claim_days * 86_400))
  end

  defp visible(endpoint, id, now) do
    Repo.one(
      from l in live(now),
        where:
          l.id == ^id and
            (l.personal_endpoint == ^endpoint or
               (l.service_endpoint == ^endpoint and
                  (is_nil(l.hidden_from) or l.hidden_from != ^endpoint)))
    )
  end

  defp link_json(%OwnerLink{} = link, service, personal, endpoint, _now) do
    confirmed = link.confirm_signature != nil
    role = if link.personal_endpoint == endpoint, do: "personal", else: "service"

    %{
      "id" => link.id,
      "role" => role,
      "service" => service,
      "personal" => personal,
      "expiresAt" => link.expires_at,
      "confirmed" => confirmed,
      "claimSignature" => link.claim_signature,
      "confirmSignature" => link.confirm_signature,
      # The service signs the personal endpoint, so it sees it. The
      # personal side sees the service endpoint once the link is active.
      "personalEndpoint" => if(role == "service", do: link.personal_endpoint),
      "serviceEndpoint" => if(role == "personal" and confirmed, do: link.service_endpoint)
    }
  end

  defp signed?(endpoint, message, signature) do
    with {:ok, raw} <- Base.url_decode64(signature, padding: false),
         {:ok, key} <- Base.decode16(endpoint, case: :lower) do
      Signature.valid?(message, raw, key)
    else
      _ -> false
    end
  end

  defp full_name(%Account{name: name, number: number}), do: Names.format(name, number)

  defp fetch_account(endpoint) do
    case Repo.get(Account, endpoint) do
      nil -> {:error, :no_account}
      account -> {:ok, account}
    end
  end
end
