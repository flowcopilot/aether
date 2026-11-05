defmodule AetherHub.ProtocolTest do
  @moduledoc """
  The hub keeps a copy of the contract in `protocol/hub/` (the release is
  built from `server/` alone). These tests check the copy and replay the
  signing vectors.
  """
  use ExUnit.Case, async: true

  import Plug.Test

  alias AetherHub.{Contract, Protocol, Signed}

  @limits Contract.get(["limits"])
  @listing @limits["listing"]
  @search @limits["search"]
  @owner_links @limits["ownerLinks"]
  @presence @limits["presence"]

  defp range(%{"min" => min, "max" => max}), do: min..max

  test "the hub speaks the protocol version, header names and error statuses of the contract" do
    assert Protocol.version() == Contract.get(["protocol"])

    assert Map.new(Protocol.headers(), fn {k, v} -> {Atom.to_string(k), v} end) ==
             Contract.get(["headers"])

    assert Map.new(Protocol.errors(), fn {k, v} -> {Atom.to_string(k), v} end) ==
             Contract.get(["errors"])
  end

  test "the hub's names and patterns are the contract's" do
    assert Protocol.kinds() == Contract.get(["names", "kinds"])
    assert Protocol.limit(:numbers) == range(Contract.get(["names", "numbers"]))

    assert Protocol.patterns() == %{
             endpoint: Contract.get(["signing", "endpoint"]),
             name: Contract.get(["names", "name"]),
             skill_id: @listing["skillId"],
             tag: @listing["tag"],
             people_query: @search["people"]["query"]
           }
  end

  test "the hub's limits are the contract's" do
    expected = %{
      max_skew_seconds: Contract.get(["signing", "maxSkewSeconds"]),
      max_body_bytes: @limits["maxBodyBytes"],
      numbers: range(Contract.get(["names", "numbers"])),
      access_days: range(@limits["accessDays"]),
      max_wait_seconds: @limits["maxWaitSeconds"],
      invite_seconds: @limits["inviteSeconds"],
      invite_prefix: @limits["invitePrefix"],
      max_invite_bytes: @limits["maxInviteBytes"],
      max_open_requests: @limits["maxOpenRequests"],
      max_listed_requests: @limits["maxListedRequests"],
      max_blocks: @limits["maxBlocks"],
      max_directory_agents: @limits["maxDirectoryAgents"],
      online_seconds: @presence["onlineSeconds"],
      last_seen_step_seconds: @presence["lastSeenStepSeconds"],
      presence_keep_seconds: @presence["keepSeconds"],
      listing_max_days: @listing["maxDays"],
      listing_max_bytes: @listing["maxBytes"],
      listing_max_description: @listing["maxDescription"],
      listing_max_skills: @listing["maxSkills"],
      listing_max_skill_name: @listing["maxSkillName"],
      listing_max_skill_description: @listing["maxSkillDescription"],
      listing_max_tags: @listing["maxTags"],
      listing_hidden_codepoints:
        Enum.map(@listing["hiddenCodepoints"], fn [lo, hi] -> lo..hi end),
      search_limit: range(@search["limit"]),
      search_default_limit: @search["limit"]["default"],
      agents_max_query_chars: @search["agents"]["maxQueryChars"],
      agents_min_word_chars: @search["agents"]["minWordChars"],
      agents_max_words: @search["agents"]["maxWords"],
      owner_link_max_days: @owner_links["maxDays"],
      owner_claim_seconds: @owner_links["claimSeconds"],
      max_open_claims: @owner_links["maxOpenClaims"],
      max_owner_links: @owner_links["maxLinks"]
    }

    assert Protocol.limits() == expected
  end

  @messages %{
    "request" => &__MODULE__.request_message/1,
    "listing" => &__MODULE__.listing_message/1,
    "ownerClaim" => &__MODULE__.owner_claim_message/1,
    "ownerConfirm" => &__MODULE__.owner_confirm_message/1
  }

  def request_message(f),
    do: Signed.request_message(f["method"], f["pathAndQuery"], f["time"], f["body"])

  def listing_message(f),
    do: Signed.listing_message(f["endpoint"], f["expiresAt"], f["listingBytes"])

  def owner_claim_message(f),
    do: Signed.owner_claim_message(f["personalEndpoint"], f["serviceHandle"], f["expiresAt"])

  def owner_confirm_message(f),
    do: Signed.owner_confirm_message(f["serviceEndpoint"], f["personalEndpoint"], f["expiresAt"])

  defp vectors do
    keys = Contract.vectors()["keys"]

    for {kind, build} <- @messages, vector <- Contract.vectors()[kind] do
      key = keys[vector["signer"]]
      {kind, build, vector, key["endpoint"], Base.decode16!(key["seed"], case: :lower)}
    end
  end

  test "every kind of signed message has vectors" do
    assert vectors() |> Enum.map(&elem(&1, 0)) |> Enum.uniq() |> Enum.sort() ==
             Enum.sort(Map.keys(Contract.get(["signing", "messages"])))
  end

  test "the vector keys are the endpoints of their seeds" do
    for {_name, key} <- Contract.vectors()["keys"] do
      seed = Base.decode16!(key["seed"], case: :lower)
      {public, _} = :crypto.generate_key(:eddsa, :ed25519, seed)
      assert Base.encode16(public, case: :lower) == key["endpoint"]
    end
  end

  test "the hub builds the message of each vector and accepts its signature" do
    for {kind, build, vector, endpoint, _seed} <- vectors() do
      assert build.(vector["fields"]) == vector["message"], "#{kind}: #{vector["name"]}"
      assert Signed.valid?(endpoint, vector["message"], vector["signature"])
    end
  end

  test "the hub refuses a vector signature for another message or another key" do
    other = Contract.vectors()["keys"]

    for {_kind, _build, vector, endpoint, _seed} <- vectors() do
      refute Signed.valid?(endpoint, vector["message"] <> " ", vector["signature"])

      for {_name, key} <- other, key["endpoint"] != endpoint do
        refute Signed.valid?(key["endpoint"], vector["message"], vector["signature"])
      end
    end
  end

  test "the hub refuses a signature or a key that does not decode" do
    [{_kind, _build, vector, endpoint, _seed} | _] = vectors()
    refute Signed.valid?(endpoint, vector["message"], "not base64url!")
    refute Signed.valid?(endpoint, vector["message"], "AAAA")
    refute Signed.valid?("not-hex", vector["message"], vector["signature"])
    refute Signed.valid?(String.duplicate("ff", 32), vector["message"], vector["signature"])
  end

  test "the test client signs the messages of the contract" do
    for {kind, _build, vector, _endpoint, seed} <- vectors() do
      assert Contract.message(kind, vector["fields"]) == vector["message"]
      assert Contract.sign(seed, vector["message"]) == vector["signature"]
    end
  end

  test "the router has every route of the contract" do
    opts = AetherHub.Router.init([])

    for %{"method" => method, "path" => path} <- Contract.get(["routes"]) do
      plug_path = String.replace(path, ~r/\{(\w+)\}/, ":\\1")
      sample = String.replace(path, ~r/\{\w+\}/, "x")
      conn = AetherHub.Router.call(conn(method, sample), opts)
      assert {^plug_path, _} = conn.private[:plug_route], "#{method} #{path}"
    end
  end
end
