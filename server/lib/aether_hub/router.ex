defmodule AetherHub.Router do
  @moduledoc """
  The HTTP routes of the hub protocol (`protocol/hub/contract.json`). Every
  rate limit is counted here: per request, per search, per directory read,
  and for the account changes and access requests that succeed.
  """
  use Plug.Router

  alias AetherHub.{Accounts, Directory, Invites, Json, Listings, Names, OwnerLinks, People}
  alias AetherHub.{Contacts, Presence, Protocol, RateLimit, Reply}

  plug :match
  plug AetherHub.ClientIp
  plug AetherHub.Verify
  plug :admit
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
        |> reply({:ok, Directory.snapshot()})

      :rate_limited ->
        reply(conn, {:error, :rate_limited})
    end
  end

  # A node reads the protocol of the hub to find an older hub, which
  # answers 404 here.
  get "/v1/hub" do
    reply(conn, {:ok, %{"protocol" => Protocol.version()}})
  end

  get "/v1/account" do
    case conn.assigns.account do
      nil -> reply(conn, {:error, :not_found})
      account -> reply(conn, {:ok, Accounts.json(account)})
    end
  end

  put "/v1/account" do
    %{endpoint: endpoint, account: account, client_ip: client_ip} = conn.assigns

    result =
      with {:ok, body} <- decode(conn) do
        counted(Accounts.quota(account, body, client_ip), fn ->
          Accounts.put(endpoint, account, body)
        end)
      end

    reply(conn, result)
  end

  delete "/v1/account" do
    reply(conn, as_account(conn, &Accounts.delete/1))
  end

  post "/v1/invites" do
    reply(conn, with_body(conn, &Invites.invite/2), 201)
  end

  get "/v1/invites" do
    conn = fetch_query_params(conn)
    reply(conn, as_account(conn, &Invites.list(&1, conn.query_params)))
  end

  post "/v1/requests" do
    result =
      with {:ok, body} <- decode(conn) do
        counted({:access_request, conn.assigns.endpoint}, fn ->
          as_account(conn, &Invites.request(&1, body))
        end)
      end

    reply(conn, result, 201)
  end

  post "/v1/invites/:id/approve" do
    body = if conn.assigns.body == "", do: {:ok, %{}}, else: decode(conn)

    result =
      with {:ok, body} <- body do
        as_account(conn, &Invites.approve(&1, id, body))
      end

    reply(conn, result)
  end

  post "/v1/invites/:id/accept" do
    reply(conn, as_account(conn, &Invites.accept(&1, id)))
  end

  post "/v1/invites/:id/bundle" do
    reply(conn, with_body(conn, &Invites.deliver(&1, id, &2)))
  end

  post "/v1/invites/:id/block" do
    reply(conn, as_account(conn, &Accounts.block_through(&1, id)))
  end

  delete "/v1/invites/:id" do
    reply(conn, as_account(conn, &Invites.delete(&1, id)))
  end

  get "/v1/contacts" do
    reply(conn, as_account(conn, &Contacts.list/1))
  end

  delete "/v1/contacts/:endpoint" do
    reply(conn, as_account(conn, &Contacts.delete(&1, endpoint)))
  end

  get "/v1/blocks" do
    reply(conn, as_account(conn, &Accounts.blocks/1))
  end

  put "/v1/blocks/:endpoint" do
    reply(conn, as_account(conn, &Accounts.block(&1, endpoint)))
  end

  delete "/v1/blocks/:endpoint" do
    reply(conn, as_account(conn, &Accounts.unblock(&1, endpoint)))
  end

  get "/v1/people/:name/:number" do
    result =
      with {:ok, {name, number}} <- parse_name(name, number),
           :ok <- count_search(conn) do
        as_account(conn, &People.lookup(&1, name, number))
      end

    reply(conn, result)
  end

  get "/v1/people" do
    conn = fetch_query_params(conn)

    result =
      with {:ok, q, limit} <- People.parse_search(conn.query_params),
           :ok <- count_search(conn) do
        as_account(conn, &People.search(&1, q, limit))
      end

    reply(conn, result)
  end

  put "/v1/listing" do
    reply(conn, with_body(conn, &Listings.put/2))
  end

  get "/v1/listing" do
    reply(conn, as_account(conn, &Listings.get/1))
  end

  delete "/v1/listing" do
    reply(conn, as_account(conn, &Listings.delete(&1.endpoint)))
  end

  get "/v1/agents/:name/:number" do
    result =
      with {:ok, {name, number}} <- parse_name(name, number),
           :ok <- count_search(conn) do
        as_account(conn, &Listings.lookup(&1, name, number))
      end

    reply(conn, result)
  end

  get "/v1/agents" do
    conn = fetch_query_params(conn)

    result =
      with {:ok, words, limit} <- Listings.parse_search(conn.query_params),
           :ok <- count_search(conn) do
        as_account(conn, &Listings.search(&1, words, limit))
      end

    reply(conn, result)
  end

  post "/v1/owner-links" do
    reply(conn, with_body(conn, &OwnerLinks.claim/2), 201)
  end

  get "/v1/owner-links" do
    reply(conn, as_account(conn, &OwnerLinks.list/1))
  end

  post "/v1/owner-links/:id/confirm" do
    reply(conn, with_body(conn, &OwnerLinks.confirm(&1, id, &2)))
  end

  delete "/v1/owner-links/:id" do
    reply(conn, as_account(conn, &OwnerLinks.delete(&1, id)))
  end

  match _ do
    reply(conn, {:error, :not_found})
  end

  # A signed request counts against the endpoint's request limit. Only a
  # request within the limit marks the endpoint as seen, and its account is
  # read once for the route.
  defp admit(%Plug.Conn{assigns: %{endpoint: endpoint}} = conn, _opts) do
    case RateLimit.hit(:request, endpoint) do
      :ok ->
        Presence.seen(endpoint)
        assign(conn, :account, Accounts.get(endpoint))

      :rate_limited ->
        conn |> Reply.error(:rate_limited) |> halt()
    end
  end

  defp admit(conn, _opts), do: conn

  defp as_account(%Plug.Conn{assigns: %{account: nil}}, _fun), do: {:error, :no_account}
  defp as_account(%Plug.Conn{assigns: %{account: account}}, fun), do: fun.(account)

  defp with_body(conn, fun) do
    with {:ok, body} <- decode(conn), do: as_account(conn, &fun.(&1, body))
  end

  # Runs `fun` when `bucket` has room for `key`, and counts a success that
  # makes something new. A request that fails uses up nothing.
  defp counted(nil, fun), do: fun.()

  defp counted({bucket, key}, fun) do
    with :ok <- limited(RateLimit.check(bucket, key)) do
      case fun.() do
        {:ok, value, :open} ->
          {:ok, value}

        {:ok, value, :created} ->
          RateLimit.hit(bucket, key)
          {:ok, value}

        {:ok, value} ->
          RateLimit.hit(bucket, key)
          {:ok, value}

        error ->
          error
      end
    end
  end

  defp count_search(conn) do
    with :ok <- limited(RateLimit.hit(:search, conn.assigns.endpoint)) do
      limited(RateLimit.hit(:search_ip, conn.assigns.client_ip))
    end
  end

  defp limited(:ok), do: :ok
  defp limited(:rate_limited), do: {:error, :rate_limited}

  # A path names an account with two segments, such as `/ana/0042`.
  defp parse_name(name, number) do
    case Names.parse_handle(name <> "#" <> number) do
      {:ok, parsed} -> {:ok, parsed}
      :error -> {:error, :invalid}
    end
  end

  defp decode(conn) do
    case Json.decode(conn.assigns.body) do
      {:ok, %{} = body} -> {:ok, body}
      _ -> {:error, :invalid}
    end
  end

  defp reply(conn, result, status \\ 200)
  defp reply(conn, :ok, _status), do: send_resp(conn, 204, "")
  defp reply(conn, {:ok, value}, status), do: Reply.json(conn, status, value)
  defp reply(conn, {:error, code}, _status), do: Reply.error(conn, code)
end
