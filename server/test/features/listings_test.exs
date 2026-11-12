defmodule AetherHub.Features.ListingsTest do
  use AetherHub.Feature, file: "listings.feature", async: false

  alias AetherHub.{Clock, Listing, Repo}

  defp search(asker, q), do: json(search_agents(asker, "q=#{q}"))["agents"]

  defp search_rows(id),
    do: Repo.query!("SELECT COUNT(*) FROM listing_search WHERE rowid = ?1", [id]).rows

  defp bad_signature(service, bytes, "signed by another key"),
    do: put_listing(service, bytes, signer: new_node())

  defp bad_signature(service, bytes, "with text that differs from the signed text"),
    do:
      put_listing(service, bytes,
        body: %{"listing" => Base.url_encode64(bytes <> " ", padding: false)}
      )

  defp bad_signature(service, bytes, "that expired a second ago"),
    do: put_listing(service, bytes, expires_at: Clock.now() - 1)

  defp bad_signature(service, bytes, "that expires in 31 days"),
    do: put_listing(service, bytes, expires_at: Clock.now() + 31 * 86_400)

  defp bad_signature(service, bytes, "that expires in 30 days"),
    do: put_listing(service, bytes, expires_at: Clock.now() + 30 * 86_400)

  defp bad_signature(service, bytes, "marked as not listed"),
    do: put_listing(service, bytes, body: %{"listed" => false})

  defp bad_signature(service, bytes, "with the expiry as text"),
    do: put_listing(service, bytes, body: %{"expiresAt" => "#{Clock.now() + 60}"})

  defp bad_signature(service, bytes, "with a malformed signature"),
    do: put_listing(service, bytes, body: %{"signature" => "AAAA"})

  defp bad_signature(service, bytes, "with an extra field"),
    do: put_listing(service, bytes, body: %{"extra" => 1})

  defp bad_signature(service, bytes, "with text that is not base64url"),
    do: put_listing(service, bytes, body: %{"listing" => "not base64url!"})

  # A listing (a map, or the exact bytes) with one problem.
  defp with_problem(service, problem) do
    base = listing(service)
    skill = hd(base["skills"])
    with_skill = fn changes -> %{base | "skills" => [Map.merge(skill, changes)]} end

    case problem do
      "an extra key" ->
        Map.put(base, "url", "https://example.com")

      "no description" ->
        Map.delete(base, "description")

      "an empty description" ->
        %{base | "description" => " "}

      "a description of 501 characters" ->
        %{base | "description" => String.duplicate("é", 501)}

      "a description of 500 characters" ->
        %{base | "description" => String.duplicate("é", 500)}

      "a control character" ->
        %{base | "description" => "one\ntwo"}

      "a bidirectional character" ->
        %{base | "description" => "abc\u202Edef"}

      "a zero-width character" ->
        %{base | "description" => "abc\u200Bdef"}

      "no skill" ->
        %{base | "skills" => []}

      "six skills" ->
        %{base | "skills" => for(n <- 1..6, do: Map.put(skill, "id", "skill-#{n}"))}

      "two skills with one id" ->
        %{base | "skills" => [skill, skill]}

      "an extra skill key" ->
        with_skill.(%{"examples" => ["x"]})

      "a bad skill id" ->
        with_skill.(%{"id" => "General"})

      "a skill name of 65 characters" ->
        with_skill.(%{"name" => String.duplicate("a", 65)})

      "a skill description of 301 characters" ->
        with_skill.(%{"description" => String.duplicate("a", 301)})

      "no tag" ->
        with_skill.(%{"tags" => []})

      "six tags" ->
        with_skill.(%{"tags" => ~w(a b c d e f)})

      "a tag two times" ->
        with_skill.(%{"tags" => ~w(a a)})

      "an upper-case tag" ->
        with_skill.(%{"tags" => ["Law"]})

      "a tag of 25 characters" ->
        with_skill.(%{"tags" => [String.duplicate("a", 25)]})

      "a number as the description" ->
        %{base | "description" => 5}

      "one key two times" ->
        ~s({"name": "#{service.handle}", "name": "#{service.handle}", ) <>
          ~s("description": "x", "skills": #{JSON.encode!(base["skills"])}})

      "a list instead of an object" ->
        "[1]"

      "text that is not JSON" ->
        "not json"

      # Valid JSON with a description of 500 characters, padded with spaces.
      "more than 8 KiB of text" ->
        JSON.encode!(%{base | "description" => String.duplicate("a", 500)}) <>
          String.duplicate(" ", 8 * 1024)
    end
  end

  # The exact bytes

  defstep ~r/^(?<name>\w+) publishes this listing text:$/,
          %{name: name, doc_string: text},
          state do
    service = actor(state, name)
    bytes = String.replace(text, "{full name}", service.handle)
    {:ok, %{conn: put_listing(service, bytes), bytes: bytes}}
  end

  defstep ~r/^the hub answers 200 with the same bytes$/, _, %{conn: conn, bytes: bytes} do
    assert conn.status == 200
    text = json(conn)["listing"]
    assert Base.url_decode64!(text, padding: false) == bytes
    {:ok, %{text: text}}
  end

  defstep ~r/^(?<name>\w+) reads its listing with the same text and a signature$/,
          %{name: name},
          %{text: text} = state do
    assert %{"listed" => true, "listing" => ^text, "signature" => signature} =
             json(request(actor(state, name), "GET", "/v1/listing"))

    {:ok, %{signature: signature}}
  end

  defstep ~r/^a person who looks up (?<name>\w+) gets the same text and signature$/,
          %{name: name},
          %{text: text, signature: signature} = state do
    path = name_path("/v1/agents", actor(state, name))

    assert %{"listing" => ^text, "signature" => ^signature} =
             json(request(claimed_node(), "GET", path))
  end

  # Who may publish

  defstep ~r/^(?<name>\w+) publishes a listing$/, %{name: name}, state do
    node = actor(state, name)
    {:ok, %{conn: put_listing(node, listing(node))}}
  end

  defstep ~r/^(?<name>\w+) publishes an empty listing$/, %{name: name}, state do
    {:ok, %{conn: request(actor(state, name), "PUT", "/v1/listing", %{})}}
  end

  # Signature, expiry and allowlist

  defstep ~r/^(?<name>\w+) publishes its listing (?<problem>.+)$/,
          %{name: name, problem: problem},
          state do
    service = actor(state, name)
    {:ok, %{conn: bad_signature(service, JSON.encode!(listing(service)), problem)}}
  end

  defstep ~r/^(?<name>\w+) has no listing$/, %{name: name}, state do
    assert request(actor(state, name), "GET", "/v1/listing").status == 404
  end

  defstep ~r/^(?<name>\w+) publishes a listing with (?<problem>.+)$/,
          %{name: name, problem: problem},
          state do
    service = actor(state, name)
    {:ok, %{conn: put_listing(service, with_problem(service, problem))}}
  end

  defstep ~r/^(?<name>\w+) publishes a listing named with (?<listed>.+)$/,
          %{name: name, listed: listed},
          state do
    service = actor(state, name)

    listed_name =
      case listed do
        "the full name of " <> other -> actor(state, other).handle
        "its name without the number" -> service.name
        "its own full name" -> service.handle
      end

    {:ok, %{conn: put_listing(service, %{listing(service) | "name" => listed_name})}}
  end

  # The end of a listing

  defstep ~r/^(?<name>\w+) has a listing that a person can find$/, %{name: name}, state do
    service = actor(state, name)
    marker = unique_name()

    assert put_listing(service, listing(service, description: marker)).status == 200
    assert [_] = search(claimed_node(), marker)
    id = Repo.get_by!(Listing, endpoint: service.endpoint).id

    {:ok, %{marker: marker, listing_id: id}}
  end

  defstep ~r/^(?<name>\w+) deletes its listing$/, %{name: name}, state do
    {:ok, %{conn: request(actor(state, name), "DELETE", "/v1/listing")}}
  end

  # The marker is the one word of the listing; no other listing has it.
  defstep ~r/^a person cannot find \w+$/, _, state do
    assert search(claimed_node(), state.marker) == []
  end

  defstep ~r/^the search index has no entry for the listing$/, _, state do
    assert search_rows(state.listing_id) == [[0]]
  end

  defstep ~r/^the hub keeps no listing for (?<name>\w+)$/, %{name: name}, state do
    assert Repo.get_by(Listing, endpoint: actor(state, name).endpoint) == nil
  end
end
