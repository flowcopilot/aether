defmodule AetherHub.Protocol do
  @moduledoc """
  The values of the hub contract that the hub uses. `protocol/hub/` at the
  repository root holds the contract; the release is built from `server/`
  alone, so the values are copied here, and `test/protocol_test.exs` checks
  the copy.
  """

  @version 2
  @headers %{
    protocol: "x-aether-protocol",
    endpoint: "x-aether-endpoint",
    time: "x-aether-time",
    signature: "x-aether-signature"
  }
  @errors %{
    invalid: 400,
    unauthorized: 401,
    no_account: 403,
    forbidden: 403,
    not_found: 404,
    taken: 409,
    limit: 409,
    too_large: 413,
    upgrade_required: 426,
    rate_limited: 429,
    unavailable: 503
  }
  @patterns %{
    endpoint: "^[0-9a-f]{64}$",
    name: "^[a-z0-9][a-z0-9._-]{0,31}$",
    skill_id: "^[a-z0-9-]{1,64}$",
    tag: "^[a-z0-9-]{1,24}$",
    people_query: "^[a-z0-9._-]{2,32}$"
  }
  @limits %{
    max_skew_seconds: 300,
    max_body_bytes: 16 * 1024,
    numbers: 1..9999,
    access_days: 1..30,
    max_wait_seconds: 30,
    invite_seconds: 7 * 86_400,
    invite_prefix: "aether-invite:",
    max_invite_bytes: 8 * 1024,
    max_open_requests: 5,
    max_listed_requests: 50,
    max_blocks: 1_000,
    max_directory_agents: 100,
    online_seconds: 45,
    last_seen_step_seconds: 900,
    presence_keep_seconds: 7 * 86_400,
    listing_max_days: 30,
    listing_max_bytes: 8 * 1024,
    listing_max_description: 500,
    listing_max_skills: 5,
    listing_max_skill_name: 64,
    listing_max_skill_description: 300,
    listing_max_tags: 5,
    listing_hidden_codepoints: [
      0x0000..0x001F,
      0x007F..0x009F,
      0x200B..0x200F,
      0x202A..0x202E,
      0x2066..0x2069,
      0xFEFF..0xFEFF
    ],
    search_limit: 1..20,
    search_default_limit: 10,
    agents_max_query_chars: 128,
    agents_min_word_chars: 2,
    agents_max_words: 8,
    owner_link_max_days: 365,
    owner_claim_seconds: 7 * 86_400,
    max_open_claims: 5,
    max_owner_links: 20
  }
  @kind %{personal: "personal", service: "service"}
  @kinds [@kind.personal, @kind.service]
  @default_kind @kind.personal
  @invite_status %{
    pending: "pending",
    requested: "requested",
    accepted: "accepted",
    delivered: "delivered"
  }
  @invite_statuses Enum.map(~w(pending requested accepted delivered)a, &@invite_status[&1])
  @invite_origin %{invite: "invite", request: "request"}
  @invite_origins [@invite_origin.invite, @invite_origin.request]
  @invite_role %{sender: "sender", receiver: "receiver"}
  @invite_roles [@invite_role.sender, @invite_role.receiver]
  @owner_link_roles @kinds

  def version, do: @version
  def headers, do: @headers
  def header(name), do: Map.fetch!(@headers, name)
  def errors, do: @errors
  def status(code), do: Map.fetch!(@errors, code)

  @doc "How long a reader may keep `/directory.json`: the route's `cacheSeconds`."
  def directory_cache_seconds, do: 15
  def kinds, do: @kinds
  def default_kind, do: @default_kind
  def kind(name), do: Map.fetch!(@kind, name)
  def invite_status(name), do: Map.fetch!(@invite_status, name)
  def invite_origin(name), do: Map.fetch!(@invite_origin, name)
  def invite_role(name), do: Map.fetch!(@invite_role, name)
  def invite_statuses, do: @invite_statuses
  def invite_origins, do: @invite_origins
  def invite_roles, do: @invite_roles
  def owner_link_roles, do: @owner_link_roles
  def limits, do: @limits
  def limit(name), do: Map.fetch!(@limits, name)
  def patterns, do: @patterns

  @doc """
  The contract pattern `name` as a regex. A contract pattern matches the
  whole value, so `$` matches only at the end, not before a final newline.
  """
  def regex(name), do: Regex.compile!(Map.fetch!(@patterns, name), [:dollar_endonly])

  @doc "`:ok` when `endpoint` is a 64-character lowercase hex endpoint id, else `{:error, :invalid}`."
  def validate_endpoint(endpoint) do
    if is_binary(endpoint) and Regex.match?(regex(:endpoint), endpoint),
      do: :ok,
      else: {:error, :invalid}
  end
end
