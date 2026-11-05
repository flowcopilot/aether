defmodule AetherHub.TestClient do
  @moduledoc """
  Builds signed requests against the router, as a node would. It signs the
  messages of `protocol/hub/contract.json` (`AetherHub.Contract`), and uses
  the time of `AetherHub.Clock` in the calling process.
  """

  import Plug.Test
  import Plug.Conn

  alias AetherHub.{Clock, Contract}

  @router_opts AetherHub.Router.init([])
  @headers Contract.get(["headers"])
  @protocol Integer.to_string(Contract.get(["protocol"]))

  @doc "A fresh node with an Ed25519 key pair; `private` is the 32-byte seed."
  def new_node do
    {public, private} = :crypto.generate_key(:eddsa, :ed25519)
    %{endpoint: Base.encode16(public, case: :lower), private: private}
  end

  @doc "A name that no other test uses."
  def unique_name, do: "n" <> Base.encode16(:crypto.strong_rand_bytes(6), case: :lower)

  @doc """
  Sends a signed request. Options:

    * `:time` - the time header value (default: the clock of this process)
    * `:signed_body` - the body to hash in the signature (default: the body)
    * `:signature` - replaces the signature header value
    * `:remote_ip` - the peer address (default: one address for each node
      key, so that async tests do not share an address bucket)
    * `:headers` - more request headers, as `{name, value}` pairs
    * `:protocol` - the protocol header value (default: the contract's);
      `nil` leaves the header out
  """
  def request(node, method, path, body \\ nil, opts \\ []) do
    raw = encode(body)
    time = Keyword.get(opts, :time, Integer.to_string(Clock.now()))

    message =
      Contract.message("request", %{
        "method" => method,
        "pathAndQuery" => path,
        "time" => time,
        "body" => Keyword.get(opts, :signed_body, raw)
      })

    conn =
      method
      |> conn(path, raw)
      |> Map.put(:remote_ip, Keyword.get_lazy(opts, :remote_ip, fn -> node_ip(node) end))
      |> put_req_header(@headers["endpoint"], node.endpoint)
      |> put_req_header(@headers["time"], time)
      |> put_req_header(@headers["signature"], Keyword.get(opts, :signature, sign(node, message)))

    conn =
      case Keyword.get(opts, :protocol, @protocol) do
        nil -> conn
        protocol -> put_req_header(conn, @headers["protocol"], protocol)
      end

    opts
    |> Keyword.get(:headers, [])
    |> Enum.reduce(conn, fn {name, value}, conn -> put_req_header(conn, name, value) end)
    |> AetherHub.Router.call(@router_opts)
  end

  @doc "Sends an unsigned request from the address `ip`."
  def public_request(method, path, ip \\ unique_ip()) do
    method |> conn(path) |> Map.put(:remote_ip, ip) |> AetherHub.Router.call(@router_opts)
  end

  @doc "A random address in 198.18.0.0/15, for a test that owns its address bucket."
  def unique_ip do
    <<a, b>> = :crypto.strong_rand_bytes(2)
    {198, 18, a, b}
  end

  defp node_ip(%{endpoint: endpoint}) do
    <<a, b, c, _::binary>> = Base.decode16!(endpoint, case: :lower)
    {10, a, b, c}
  end

  @doc "The decoded JSON body of a response."
  def json(conn), do: JSON.decode!(conn.resp_body)

  @doc """
  A fresh node with a fresh name of `kind`. It has `:handle` (the full name,
  such as `n1a2b#0042`), `:name` and `:number`.
  """
  def claimed_node(kind \\ "personal") do
    node = new_node()
    conn = request(node, "PUT", "/v1/account", %{"name" => unique_name(), "kind" => kind})
    %{status: 200} = conn
    with_account(node, json(conn))
  end

  @doc "`node` with the full name, the name and the number of `account`."
  def with_account(node, account),
    do:
      Map.merge(node, %{
        handle: account["handle"],
        name: account["name"],
        number: account["number"]
      })

  @doc """
  The path of the account `node` under `base`, such as `/v1/people`: the
  name and the number as two segments.
  """
  def name_path(base, node) do
    [name, number] = String.split(node.handle, "#")
    "#{base}/#{name}/#{number}"
  end

  @doc """
  The listing of `node` with one skill. Options: `:description`, `:skill`
  (the skill name), `:skill_description` and `:tags`.
  """
  def listing(node, opts \\ []) do
    %{
      "name" => node.handle,
      "description" => Keyword.get(opts, :description, "Answers questions about a service."),
      "skills" => [
        %{
          "id" => "general",
          "name" => Keyword.get(opts, :skill, "General"),
          "description" => Keyword.get(opts, :skill_description, "Does tasks in its field."),
          "tags" => Keyword.get(opts, :tags, ["general"])
        }
      ]
    }
  end

  @doc """
  Sends `listing` (a map, or the exact bytes) for `node` with
  `PUT /v1/listing`. Options:

    * `:expires_at` - Unix time (default: in 7 days)
    * `:signer` - the node whose key signs (default: `node`)
    * `:body` - keys that replace or add to the request body
  """
  def put_listing(node, listing, opts \\ []) do
    bytes = if is_binary(listing), do: listing, else: JSON.encode!(listing)
    expires_at = Keyword.get(opts, :expires_at, Clock.now() + 7 * 86_400)

    message =
      Contract.message("listing", %{
        "endpoint" => node.endpoint,
        "expiresAt" => expires_at,
        "listingBytes" => bytes
      })

    body =
      Map.merge(
        %{
          "listing" => Base.url_encode64(bytes, padding: false),
          "listed" => true,
          "expiresAt" => expires_at,
          "signature" => sign(Keyword.get(opts, :signer, node), message)
        },
        Keyword.get(opts, :body, %{})
      )

    request(node, "PUT", "/v1/listing", body)
  end

  @doc """
  `personal` claims the service agent with the full name `service`. Options:
  `:expires_at` (default: in 30 days), `:signer` (default: `personal`) and
  `:signed_name` (the full name in the signed message, default: `service`).
  """
  def claim_owner(personal, service, opts \\ []) do
    expires_at = Keyword.get(opts, :expires_at, Clock.now() + 30 * 86_400)

    message =
      Contract.message("ownerClaim", %{
        "personalEndpoint" => personal.endpoint,
        "serviceHandle" => Keyword.get(opts, :signed_name, service),
        "expiresAt" => expires_at
      })

    request(personal, "POST", "/v1/owner-links", %{
      "service" => service,
      "expiresAt" => expires_at,
      "signature" => sign(Keyword.get(opts, :signer, personal), message)
    })
  end

  @doc """
  `service` confirms the claim `link` (as the service sees it). Options:
  `:signer`, `:expires_at` and `:personal` (the endpoint in the signed
  message) replace the values of the link.
  """
  def confirm_owner(service, link, opts \\ []) do
    message =
      Contract.message("ownerConfirm", %{
        "serviceEndpoint" => service.endpoint,
        "personalEndpoint" => Keyword.get(opts, :personal, link["personalEndpoint"]),
        "expiresAt" => Keyword.get(opts, :expires_at, link["expiresAt"])
      })

    request(service, "POST", "/v1/owner-links/#{link["id"]}/confirm", %{
      "signature" => sign(Keyword.get(opts, :signer, service), message)
    })
  end

  @doc "The unpadded base64url Ed25519 signature of `message` by `node`."
  def sign(node, message), do: Contract.sign(node.private, message)

  defp encode(nil), do: ""
  defp encode(body) when is_binary(body), do: body
  defp encode(body), do: JSON.encode!(body)
end
