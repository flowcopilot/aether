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

  alias AetherHub.{Account, Blocks, Clock, Names, OwnerLink, Protocol, Repo, Signed}

  @max_seconds Protocol.limit(:owner_link_max_days) * 86_400
  @claim_seconds Protocol.limit(:owner_claim_seconds)
  @max_open_claims Protocol.limit(:max_open_claims)
  @max_links Protocol.limit(:max_owner_links)
  @personal Protocol.kind(:personal)
  @service Protocol.kind(:service)

  @doc """
  The personal agent `account` claims the service agent in `body`. A new
  claim for the same service replaces the old link, which the service must
  confirm again.
  """
  def claim(
        %Account{} = me,
        %{"service" => service, "expiresAt" => expires_at, "signature" => signature} = body
      )
      when map_size(body) == 3 and is_binary(service) and is_integer(expires_at) and
             is_binary(signature) do
    now = Clock.now()

    Repo.atomic(fn ->
      if me.kind != @personal, do: Repo.rollback(:forbidden)

      target = Repo.ok!(Names.find(service, @service))

      message = Signed.owner_claim_message(me.endpoint, service, expires_at)

      unless expires_at > now and expires_at <= now + @max_seconds and
               Signed.valid?(me.endpoint, message, signature),
             do: Repo.rollback(:invalid)

      within_limits!(me.endpoint, target.endpoint)

      Repo.delete_all(
        from l in OwnerLink,
          where: l.service_endpoint == ^target.endpoint and l.personal_endpoint == ^me.endpoint
      )

      Repo.insert!(%OwnerLink{
        id: Repo.new_id(),
        service_endpoint: target.endpoint,
        personal_endpoint: me.endpoint,
        service_handle: service,
        expires_at: expires_at,
        claim_signature: signature,
        hidden_from: Blocks.hidden_from(target.endpoint, me.endpoint),
        created_at: now
      })
      |> json(Account.full_name(target), Account.full_name(me), me.endpoint)
    end)
  end

  def claim(%Account{}, _body), do: {:error, :invalid}

  defp within_limits!(personal, service) do
    others =
      from(l in OwnerLink,
        where: l.personal_endpoint == ^personal and l.service_endpoint != ^service
      )

    open =
      Repo.aggregate(
        from(l in others, where: is_nil(l.confirm_signature) and l.created_at > ^claim_cutoff()),
        :count
      )

    if open >= @max_open_claims or Repo.aggregate(others, :count) >= @max_links,
      do: Repo.rollback(:limit)
  end

  @doc "The service agent `account` confirms the claim `id` with the signature in `body`."
  def confirm(%Account{endpoint: endpoint} = service, id, %{"signature" => signature} = body)
      when map_size(body) == 1 and is_binary(signature) do
    Repo.atomic(fn ->
      link =
        Repo.one(from l in seen_by(endpoint), where: l.id == ^id) || Repo.rollback(:not_found)

      unless link.service_endpoint == endpoint and is_nil(link.confirm_signature),
        do: Repo.rollback(:forbidden)

      message = Signed.owner_confirm_message(endpoint, link.personal_endpoint, link.expires_at)
      unless Signed.valid?(endpoint, message, signature), do: Repo.rollback(:invalid)
      if active_owner(endpoint), do: Repo.rollback(:limit)

      link = link |> Ecto.Changeset.change(confirm_signature: signature) |> Repo.update!()
      personal = Repo.get!(Account, link.personal_endpoint)
      json(link, Account.full_name(service), Account.full_name(personal), endpoint)
    end)
  end

  def confirm(%Account{}, _id, _body), do: {:error, :invalid}

  @doc "The links and claims that `account` sees."
  def list(%Account{endpoint: endpoint}) do
    links =
      from(l in seen_by(endpoint),
        join: s in Account,
        on: s.endpoint == l.service_endpoint,
        join: p in Account,
        on: p.endpoint == l.personal_endpoint,
        order_by: [asc: l.created_at, asc: l.id],
        select: {l, s, p}
      )
      |> Repo.all()
      |> Enum.map(fn {link, service, personal} ->
        json(link, Account.full_name(service), Account.full_name(personal), endpoint)
      end)

    {:ok, %{"links" => links}}
  end

  @doc "Either side removes the link `id`."
  def delete(%Account{endpoint: endpoint}, id) do
    {count, _} =
      Repo.delete_all(from l in OwnerLink, where: l.id == ^id, where: ^sides(endpoint))

    if count > 0, do: :ok, else: {:error, :not_found}
  end

  def delete_for(endpoint) do
    Repo.delete_all(
      from l in OwnerLink,
        where: l.service_endpoint == ^endpoint or l.personal_endpoint == ^endpoint
    )

    :ok
  end

  def delete_between(one, other) do
    Repo.delete_all(
      from l in OwnerLink,
        where:
          (l.service_endpoint == ^one and l.personal_endpoint == ^other) or
            (l.service_endpoint == ^other and l.personal_endpoint == ^one)
    )

    :ok
  end

  @doc "The full name of the active owner of the service agent `service`, or `nil`."
  def active_owner(service) do
    from(l in OwnerLink,
      join: p in Account,
      on: p.endpoint == l.personal_endpoint,
      where:
        l.service_endpoint == ^service and not is_nil(l.confirm_signature) and
          l.expires_at > ^Clock.now(),
      select: p,
      limit: 1
    )
    |> Repo.one()
    |> case do
      nil -> nil
      personal -> Account.full_name(personal)
    end
  end

  @doc "Removes the expired links, and the claims that waited too long."
  def delete_expired do
    Repo.delete_all(
      from l in OwnerLink,
        where:
          l.expires_at <= ^Clock.now() or
            (is_nil(l.confirm_signature) and l.created_at <= ^claim_cutoff())
    )

    :ok
  end

  defp seen_by(endpoint) do
    from l in OwnerLink,
      where:
        l.expires_at > ^Clock.now() and
          (not is_nil(l.confirm_signature) or l.created_at > ^claim_cutoff()),
      where: ^sides(endpoint)
  end

  # The personal side sees every link it claimed; the service side does not
  # see a claim from a node that it blocked.
  defp sides(endpoint),
    do:
      dynamic(
        [l],
        l.personal_endpoint == ^endpoint or
          (l.service_endpoint == ^endpoint and ^Blocks.visible_to(endpoint))
      )

  defp claim_cutoff, do: Clock.now() - @claim_seconds

  defp json(%OwnerLink{} = link, service, personal, endpoint) do
    confirmed = link.confirm_signature != nil
    role = if link.personal_endpoint == endpoint, do: @personal, else: @service

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
      "personalEndpoint" => if(role == @service, do: link.personal_endpoint),
      "serviceEndpoint" => if(role == @personal and confirmed, do: link.service_endpoint)
    }
  end
end
