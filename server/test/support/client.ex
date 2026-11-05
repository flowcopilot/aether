defmodule AetherHub.TestClient do
  @moduledoc "Builds signed requests against the router, as a node would."

  import Plug.Test
  import Plug.Conn

  @router_opts AetherHub.Router.init([])

  @doc "A fresh node with an Ed25519 key pair."
  def new_node do
    {public, private} = :crypto.generate_key(:eddsa, :ed25519)
    %{endpoint: Base.encode16(public, case: :lower), private: private}
  end

  @doc "A name that no other test uses."
  def unique_name, do: "n" <> Base.encode16(:crypto.strong_rand_bytes(6), case: :lower)

  @doc """
  Sends a signed request. Options:

    * `:time` - the `x-aether-time` value (default: now)
    * `:signed_body` - the body to hash in the signature (default: the body)
    * `:signature` - replaces the signature header value
    * `:remote_ip` - the peer address (default: one address for each node
      key, so that async tests do not share an address bucket)
    * `:headers` - more request headers, as `{name, value}` pairs
    * `:protocol` - the `x-aether-protocol` value (default `"2"`); `nil`
      leaves the header out
  """
  def request(node, method, path, body \\ nil, opts \\ []) do
    raw = encode(body)
    time = Keyword.get(opts, :time, Integer.to_string(System.system_time(:second)))
    signed_body = Keyword.get(opts, :signed_body, raw)
    message = AetherHub.Verify.message(method, path, time, signed_body)
    signature = :crypto.sign(:eddsa, :none, message, [node.private, :ed25519])

    signature =
      Keyword.get(opts, :signature, Base.url_encode64(signature, padding: false))

    conn =
      method
      |> conn(path, raw)
      |> Map.put(:remote_ip, Keyword.get_lazy(opts, :remote_ip, fn -> node_ip(node) end))
      |> put_req_header("x-aether-endpoint", node.endpoint)
      |> put_req_header("x-aether-time", time)
      |> put_req_header("x-aether-signature", signature)

    conn =
      case Keyword.get(opts, :protocol, "2") do
        nil -> conn
        protocol -> put_req_header(conn, "x-aether-protocol", protocol)
      end

    opts
    |> Keyword.get(:headers, [])
    |> Enum.reduce(conn, fn {name, value}, conn -> put_req_header(conn, name, value) end)
    |> AetherHub.Router.call(@router_opts)
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
    account = json(conn)

    Map.merge(node, %{
      handle: account["handle"],
      name: account["name"],
      number: account["number"]
    })
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
    expires_at = Keyword.get(opts, :expires_at, System.system_time(:second) + 7 * 86_400)
    signer = Keyword.get(opts, :signer, node)
    message = AetherHub.Listings.message(node.endpoint, expires_at, bytes)
    signature = :crypto.sign(:eddsa, :none, message, [signer.private, :ed25519])

    body =
      Map.merge(
        %{
          "listing" => Base.url_encode64(bytes, padding: false),
          "listed" => true,
          "expiresAt" => expires_at,
          "signature" => Base.url_encode64(signature, padding: false)
        },
        Keyword.get(opts, :body, %{})
      )

    request(node, "PUT", "/v1/listing", body)
  end

  @doc "The unpadded base64url Ed25519 signature of `message` by `node`."
  def sign(node, message) do
    :crypto.sign(:eddsa, :none, message, [node.private, :ed25519])
    |> Base.url_encode64(padding: false)
  end

  defp encode(nil), do: ""
  defp encode(body) when is_binary(body), do: body
  defp encode(body), do: JSON.encode!(body)
end
