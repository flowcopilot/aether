defmodule AetherHub.Features.OwnerLinksTest do
  use AetherHub.Feature, file: "owner-links.feature", async: true

  alias AetherHub.{Clock, Repo}

  @day 86_400

  defp in_days(days), do: Clock.now() + days * @day

  defp claim(personal, service, opts \\ []), do: claim_owner(personal, service.handle, opts)

  defp claim_opts(""), do: []
  defp claim_opts("signed by another key"), do: [signer: claimed_node()]
  defp claim_opts("for another name"), do: [signed_name: "other#0001"]
  defp claim_opts("that expires in 366 days"), do: [expires_at: in_days(366)]
  defp claim_opts("that expired a day ago"), do: [expires_at: in_days(-1)]
  defp claim_opts("for 365 days"), do: [expires_at: in_days(365)]

  defp confirm_opts(_service, _link, ""), do: []

  defp confirm_opts(_service, _link, "signed by another key"),
    do: [signer: claimed_node("service")]

  defp confirm_opts(_service, link, "with another expiry"),
    do: [expires_at: link["expiresAt"] + 1]

  defp confirm_opts(service, _link, "for another personal agent"),
    do: [personal: service.endpoint]

  defp links(node), do: json(request(node, "GET", "/v1/owner-links"))["links"]

  defp lookup(service), do: json(request(claimed_node(), "GET", name_path("/v1/agents", service)))

  # The claim of `personal` as the claimed `service` sees it. When the service does not
  # see it (it blocked the person), the id that the person sees, with a made-up
  # expiry.
  defp link(state, service, personal) do
    service = actor(state, service)
    personal = actor(state, personal)

    case Enum.find(links(service), &(&1["personal"] == personal.handle)) do
      nil ->
        assert [%{"id" => id}] = links(personal)
        %{"id" => id, "expiresAt" => 1, "personalEndpoint" => personal.endpoint}

      link ->
        link
    end
  end

  defp claimer(state, ""), do: state.claimer
  defp claimer(_state, name), do: name

  defp listed_count(service) do
    Repo.query!("SELECT COUNT(*) FROM owner_links WHERE service_endpoint = ?1", [
      service.endpoint
    ])
  end

  # Claims

  defstep ~r/^(?<who>\w+) claims (?<service>\w+)(?: (?<problem>signed by another key|for another name|that expires in 366 days|that expired a day ago|for 365 days))?$/,
          %{who: who, service: service, problem: problem},
          state do
    conn = claim(actor(state, who), actor(state, service), claim_opts(problem))
    {:ok, %{conn: conn, claimer: who, claimed: service}}
  end

  defstep ~r/^(?<who>\w+) claimed (?<service>\w+)(?<days> for 365 days)?$/,
          %{who: who, service: service, days: days},
          state do
    opts = claim_opts(String.trim(days))
    assert claim(actor(state, who), actor(state, service), opts).status == 201
    {:ok, %{claimer: who, claimed: service}}
  end

  defstep ~r/^(?<who>\w+) claims a person$/, %{who: who}, state do
    {:ok, %{conn: claim(actor(state, who), claimed_node())}}
  end

  defstep ~r/^(?<who>\w+) claims the name "(?<handle>[^"]+)"$/,
          %{who: who, handle: handle},
          state do
    {:ok, %{conn: claim(actor(state, who), %{handle: handle})}}
  end

  defstep ~r/^(?<who>\w+) has open claims on 5 service agents$/, %{who: who}, state do
    node = actor(state, who)
    for _ <- 1..5, do: assert(claim(node, listed_service()).status == 201)
  end

  defstep ~r/^(?<who>\w+) claims one more service agent$/, %{who: who}, state do
    {:ok, %{conn: claim(actor(state, who), listed_service())}}
  end

  defstep ~r/^(?<who>\w+) has 20 links$/, %{who: who}, state do
    node = actor(state, who)
    for _ <- 1..20, do: link_owner(node, claimed_node("service"))
  end

  # Confirmations

  defstep ~r/^(?<service>\w+) confirms the claim(?: of (?<who>\w+))?(?: (?<problem>signed by another key|with another expiry|for another personal agent))?$/,
          %{service: service, who: who, problem: problem},
          state do
    link = link(state, state.claimed, claimer(state, who))
    node = actor(state, service)
    {:ok, %{conn: confirm_owner(node, link, confirm_opts(node, link, problem))}}
  end

  defstep ~r/^(?<service>\w+) confirmed the claim(?: of (?<who>\w+))?$/,
          %{service: service, who: who},
          state do
    link = link(state, service, claimer(state, who))
    assert confirm_owner(actor(state, service), link).status == 200
  end

  defstep ~r/^(?<who>\w+) confirms (?:her|his) own claim$/, %{who: who}, state do
    node = actor(state, who)
    [own] = links(node)
    {:ok, %{conn: confirm_owner(node, own, personal: node.endpoint)}}
  end

  # What each side sees

  defstep ~r/^the answer is an open claim for the person, without the endpoint of \w+$/,
          _,
          %{conn: conn} do
    assert %{"role" => "personal", "confirmed" => false, "serviceEndpoint" => nil} = json(conn)
  end

  defstep ~r/^the answer is a confirmed link$/, _, %{conn: conn} do
    assert json(conn)["confirmed"] == true
  end

  defstep ~r/^(?<service>\w+) sees the claim from (?<who>\w+), with the endpoint of \k<who>$/,
          %{service: service, who: who},
          state do
    person = actor(state, who)

    assert [%{"role" => "service", "personal" => personal, "personalEndpoint" => endpoint}] =
             links(actor(state, service))

    assert {personal, endpoint} == {person.handle, person.endpoint}
  end

  defstep ~r/^(?<who>\w+) sees the confirmed link, with the endpoint of (?<service>\w+)$/,
          %{who: who, service: service},
          state do
    assert [%{"confirmed" => true, "serviceEndpoint" => endpoint}] = links(actor(state, who))
    assert endpoint == actor(state, service).endpoint
  end

  defstep ~r/^(?<service>\w+) sees the claim still open$/, %{service: service}, state do
    assert [%{"confirmed" => false}] = links(actor(state, service))
  end

  defstep ~r/^(?<who>\w+) sees (?:her|his) claim$/, %{who: who}, state do
    assert [%{"id" => _}] = links(actor(state, who))
  end

  defstep ~r/^(?<who>\w+) has no links$/, %{who: who}, state do
    assert links(actor(state, who)) == []
  end

  defstep ~r/^a lookup of (?<service>\w+) shows no owner$/, %{service: service}, state do
    assert lookup(actor(state, service))["owner"] == nil
  end

  defstep ~r/^a lookup of (?<service>\w+) shows (?<owner>\w+) as the owner(?<hidden>, without the endpoint of \k<owner>)?$/,
          %{service: service, owner: owner, hidden: hidden},
          state do
    owner = actor(state, owner)
    row = lookup(actor(state, service))
    assert row["owner"] == owner.handle
    if hidden != "", do: refute(String.contains?(JSON.encode!(row), owner.endpoint))
  end

  # The end of a link

  defstep ~r/^(?<service>\w+) deletes the link of (?<who>\w+)$/,
          %{service: service, who: who},
          state do
    %{"id" => id} = link(state, service, who)
    {:ok, %{conn: request(actor(state, service), "DELETE", "/v1/owner-links/#{id}")}}
  end

  defstep ~r/^(?<who>\w+) deletes (?:her|his) link$/, %{who: who}, state do
    assert [%{"id" => id}] = links(actor(state, who))
    {:ok, %{conn: request(actor(state, who), "DELETE", "/v1/owner-links/#{id}")}}
  end

  # Later steps of this scenario read at the expiry.
  defstep ~r/^the link expires$/, _, state do
    Clock.set(link(state, state.claimed, state.claimer)["expiresAt"])
  end

  defstep ~r/^the claim of \w+ is 8 days old$/, _, _state do
    Clock.set(in_days(8))
  end

  defstep ~r/^the hub keeps no links for (?<service>\w+)$/, %{service: service}, state do
    assert %{rows: [[0]]} = listed_count(actor(state, service))
  end
end
