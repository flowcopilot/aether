defmodule AetherHub.Router do
  @moduledoc "The HTTP routes of the hub protocol, version 1."
  use Plug.Router

  alias AetherHub.{Directory, Hub, Listings, OwnerLinks, People, RateLimit}

  plug :match
  plug AetherHub.ClientIp
  plug AetherHub.Verify
  plug :dispatch

  get "/healthz" do
    conn |> put_resp_content_type("text/plain") |> send_resp(200, "ok")
  end

  @page_path Path.expand("../../priv/static/index.html", __DIR__)
  @external_resource @page_path
  @page File.read!(@page_path)
  # The page has its script and style inline, and loads nothing from
  # another origin.
  @page_policy "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; " <>
                 "connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"

  # The public landing page. It reads `/directory.json` and needs no account.
  get "/" do
    conn
    |> put_resp_header("content-security-policy", @page_policy)
    |> put_resp_header("x-content-type-options", "nosniff")
    |> put_resp_header("referrer-policy", "no-referrer")
    |> put_resp_content_type("text/html")
    |> send_resp(200, @page)
  end

  @docs_path Path.expand("../../priv/static/docs.md", __DIR__)
  @external_resource @docs_path
  @docs File.read!(@docs_path)

  # Setup steps in Markdown, for an agent that installs Aether for its user.
  get "/docs" do
    conn
    |> put_resp_header("x-content-type-options", "nosniff")
    |> put_resp_header("cache-control", "public, max-age=300")
    |> put_resp_content_type("text/markdown")
    |> send_resp(200, @docs)
  end

  get "/directory.json" do
    case RateLimit.hit(:directory_ip, conn.assigns.client_ip) do
      :ok ->
        conn
        |> put_resp_header("cache-control", "public, max-age=15")
        |> reply({:ok, Directory.snapshot(now())})

      :rate_limited ->
        reply(conn, {:error, :rate_limited})
    end
  end

  # The protocol of this hub. A node reads it to find an older hub, which
  # answers 404 here.
  get "/v1/hub" do
    reply(conn, {:ok, %{"protocol" => 2}})
  end

  get "/v1/account" do
    conn |> reply(Hub.get_account(conn.assigns.endpoint))
  end

  delete "/v1/account" do
    case Hub.delete_account(conn.assigns.endpoint) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  put "/v1/account" do
    with {:ok, body} <- decode(conn) do
      reply(conn, Hub.put_account(conn.assigns.endpoint, body, conn.assigns.client_ip))
    else
      error -> reply(conn, error)
    end
  end

  post "/v1/invites" do
    with {:ok, body} <- decode(conn) do
      reply(conn, Hub.create_invite(conn.assigns.endpoint, body), 201)
    else
      error -> reply(conn, error)
    end
  end

  get "/v1/invites" do
    conn = fetch_query_params(conn)
    reply(conn, Hub.list_invites(conn.assigns.endpoint, conn.query_params))
  end

  post "/v1/requests" do
    with {:ok, body} <- decode(conn) do
      reply(conn, Hub.create_request(conn.assigns.endpoint, body), 201)
    else
      error -> reply(conn, error)
    end
  end

  post "/v1/invites/:id/approve" do
    body = if conn.assigns.body == "", do: {:ok, %{}}, else: decode(conn)

    with {:ok, body} <- body do
      reply(conn, Hub.approve_request(conn.assigns.endpoint, id, body))
    else
      error -> reply(conn, error)
    end
  end

  post "/v1/invites/:id/accept" do
    reply(conn, Hub.accept_invite(conn.assigns.endpoint, id))
  end

  post "/v1/invites/:id/bundle" do
    with {:ok, body} <- decode(conn) do
      reply(conn, Hub.deliver_bundle(conn.assigns.endpoint, id, body))
    else
      error -> reply(conn, error)
    end
  end

  post "/v1/invites/:id/block" do
    case Hub.block_invite(conn.assigns.endpoint, id) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  get "/v1/blocks" do
    reply(conn, Hub.list_blocks(conn.assigns.endpoint))
  end

  put "/v1/blocks/:blocked" do
    case Hub.block(conn.assigns.endpoint, blocked) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  delete "/v1/blocks/:blocked" do
    case Hub.unblock(conn.assigns.endpoint, blocked) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  delete "/v1/invites/:id" do
    case Hub.delete_invite(conn.assigns.endpoint, id) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  get "/v1/contacts" do
    reply(conn, Hub.list_contacts(conn.assigns.endpoint))
  end

  delete "/v1/contacts/:contact" do
    case Hub.delete_contact(conn.assigns.endpoint, contact) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  get "/v1/people/:name/:number" do
    with {:ok, {name, number}} <- parse_name(name, number),
         :ok <- count_search(conn) do
      reply(conn, People.lookup(conn.assigns.endpoint, name, number, now()))
    else
      error -> reply(conn, error)
    end
  end

  get "/v1/people" do
    conn = fetch_query_params(conn)

    with {:ok, q, limit} <- People.parse_search(conn.query_params),
         :ok <- count_search(conn) do
      reply(conn, People.search(conn.assigns.endpoint, q, limit, now()))
    else
      error -> reply(conn, error)
    end
  end

  put "/v1/listing" do
    case Listings.decode_json(conn.assigns.body) do
      {:ok, body} -> reply(conn, Listings.put(conn.assigns.endpoint, body, now()))
      :error -> reply(conn, {:error, :invalid})
    end
  end

  get "/v1/listing" do
    reply(conn, Listings.get(conn.assigns.endpoint, now()))
  end

  delete "/v1/listing" do
    case Hub.delete_listing(conn.assigns.endpoint) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  get "/v1/agents/:name/:number" do
    with {:ok, {name, number}} <- parse_name(name, number),
         :ok <- count_search(conn) do
      reply(conn, Listings.lookup(conn.assigns.endpoint, name, number, now()))
    else
      error -> reply(conn, error)
    end
  end

  get "/v1/agents" do
    conn = fetch_query_params(conn)

    with {:ok, words, limit} <- Listings.parse_search(conn.query_params),
         :ok <- count_search(conn) do
      reply(conn, Listings.search(conn.assigns.endpoint, words, limit, now()))
    else
      error -> reply(conn, error)
    end
  end

  post "/v1/owner-links" do
    with {:ok, body} <- decode(conn) do
      reply(conn, OwnerLinks.claim(conn.assigns.endpoint, body, now()), 201)
    else
      error -> reply(conn, error)
    end
  end

  get "/v1/owner-links" do
    reply(conn, OwnerLinks.list(conn.assigns.endpoint, now()))
  end

  post "/v1/owner-links/:id/confirm" do
    with {:ok, body} <- decode(conn) do
      reply(conn, OwnerLinks.confirm(conn.assigns.endpoint, id, body, now()))
    else
      error -> reply(conn, error)
    end
  end

  delete "/v1/owner-links/:id" do
    case OwnerLinks.delete(conn.assigns.endpoint, id) do
      :ok -> send_resp(conn, 204, "")
      error -> reply(conn, error)
    end
  end

  match _ do
    error(conn, 404, "not_found")
  end

  # A path names an account with two segments, such as `/ana/0042`.
  defp parse_name(name, number) do
    case AetherHub.Names.parse_handle(name <> "#" <> number) do
      {:ok, parsed} -> {:ok, parsed}
      :error -> {:error, :invalid}
    end
  end

  # A search counts for the endpoint and for the client address.
  defp count_search(conn) do
    with :ok <- RateLimit.hit(:search, conn.assigns.endpoint),
         :ok <- RateLimit.hit(:search_ip, conn.assigns.client_ip) do
      :ok
    else
      :rate_limited -> {:error, :rate_limited}
    end
  end

  defp now, do: System.system_time(:second)

  @statuses %{
    invalid: 400,
    no_account: 403,
    forbidden: 403,
    not_found: 404,
    taken: 409,
    limit: 409,
    upgrade_required: 426,
    rate_limited: 429
  }

  defp reply(conn, result, status \\ 200)

  defp reply(conn, {:ok, value}, status) do
    conn |> put_resp_content_type("application/json") |> send_resp(status, JSON.encode!(value))
  end

  defp reply(conn, {:error, code}, _status) do
    error(conn, Map.fetch!(@statuses, code), Atom.to_string(code))
  end

  @doc false
  def error(conn, status, code) do
    conn
    |> put_resp_content_type("application/json")
    |> send_resp(status, JSON.encode!(%{"error" => code}))
  end

  defp decode(conn) do
    case JSON.decode(conn.assigns.body) do
      {:ok, %{} = body} -> {:ok, body}
      _ -> {:error, :invalid}
    end
  end
end
