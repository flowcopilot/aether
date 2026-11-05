defmodule AetherHub.Listings do
  @moduledoc """
  Public listings of service agents, and the search on their text.

  A node sends its listing as base64url text with an Ed25519 signature of
  the endpoint key. The hub checks the text, keeps it exactly as sent, and
  returns it as sent: a node can check the bytes again. Only the full name,
  the description, and the skills are allowed, so a listing holds no
  endpoint and no address.
  """

  import Ecto.Query

  alias AetherHub.{Account, Contact, Listing, Names, Presence, Repo, Signature}

  @max_days 30
  @max_bytes 8 * 1024
  @body_keys ["expiresAt", "listed", "listing", "signature"]
  @listing_keys ["description", "name", "skills"]
  @skill_keys ["description", "id", "name", "tags"]
  @max_description 500
  @max_skill_name 64
  @max_skill_description 300
  @max_skills 5
  @max_tags 5
  @tag ~r/\A[a-z0-9-]{1,24}\z/
  @skill_id ~r/\A[a-z0-9-]{1,64}\z/
  # Control characters, and characters with no width or that change the
  # direction of the text.
  @hidden ~r/[\x{0000}-\x{001F}\x{007F}-\x{009F}\x{200B}-\x{200F}\x{202A}-\x{202E}\x{2066}-\x{2069}\x{FEFF}]/u
  @max_query 128
  @max_tokens 8
  @default_limit 10
  @max_limit 20

  ## JSON

  @doc """
  Decodes `text` as JSON, like `JSON.decode/1`, but refuses an object with a
  key two times. Two readers of such an object can see two different values.
  """
  def decode_json(text) when is_binary(text) do
    case JSON.decode(text, :ok, object_push: &push/3, object_finish: &finish/2) do
      {value, :ok, ""} -> {:ok, value}
      _ -> :error
    end
  catch
    :duplicate_key -> :error
  end

  def decode_json(_text), do: :error

  defp push(key, value, pairs), do: [{key, value} | pairs]

  defp finish(pairs, old) do
    map = Map.new(pairs)
    if map_size(map) != length(pairs), do: throw(:duplicate_key)
    {map, old}
  end

  ## The listing of this node

  @doc "The message that a node signs for its listing."
  def message(endpoint, expires_at, bytes) do
    Enum.join(
      [
        "aether-listing/v1",
        endpoint,
        "true",
        Integer.to_string(expires_at),
        Base.encode16(:crypto.hash(:sha256, bytes), case: :lower)
      ],
      "\n"
    )
  end

  @doc """
  Keeps the listing in `body` for the service agent `endpoint` at Unix time
  `now`, and updates the search index in the same transaction.
  """
  def put(endpoint, body, now) do
    with {:ok, account} <- fetch_account(endpoint),
         :ok <- service(account),
         {:ok, text, bytes, signature, expires_at} <- signed_body(endpoint, body, now),
         {:ok, fields} <- parse(bytes, full_name(account)) do
      Repo.transact(fn ->
        listing =
          case Repo.get_by(Listing, endpoint: endpoint) do
            nil -> %Listing{endpoint: endpoint}
            listing -> listing
          end
          |> Ecto.Changeset.change(
            listing: text,
            signature: signature,
            expires_at: expires_at,
            updated_at: now
          )
          |> Repo.insert_or_update!()

        index(listing.id, fields)
        {:ok, listing_json(listing)}
      end)
    end
  end

  defp service(%Account{kind: "service"}), do: :ok
  defp service(_account), do: {:error, :invalid}

  defp signed_body(endpoint, %{} = body, now) do
    with true <- Enum.sort(Map.keys(body)) == @body_keys,
         true <- body["listed"] == true,
         expires_at when is_integer(expires_at) <- body["expiresAt"],
         true <- expires_at > now and expires_at <= now + @max_days * 86_400,
         text when is_binary(text) <- body["listing"],
         {:ok, bytes} <- Base.url_decode64(text, padding: false),
         true <- byte_size(bytes) <= @max_bytes,
         signature when is_binary(signature) <- body["signature"],
         {:ok, raw_signature} <- Base.url_decode64(signature, padding: false),
         key = Base.decode16!(endpoint, case: :lower),
         true <- Signature.valid?(message(endpoint, expires_at, bytes), raw_signature, key) do
      {:ok, text, bytes, signature, expires_at}
    else
      _ -> {:error, :invalid}
    end
  end

  defp signed_body(_endpoint, _body, _now), do: {:error, :invalid}

  # The text fields of the listing `bytes` for the search index, when the
  # listing passes the allowlist and names `handle`.
  defp parse(bytes, handle) do
    with {:ok, %{} = listing} <- decode_json(bytes),
         true <- Enum.sort(Map.keys(listing)) == @listing_keys,
         true <- listing["name"] == handle,
         true <- text?(listing["description"], @max_description),
         skills when is_list(skills) and skills != [] and length(skills) <= @max_skills <-
           listing["skills"],
         true <- Enum.all?(skills, &skill?/1),
         true <- unique?(Enum.map(skills, & &1["id"])) do
      {:ok,
       %{
         name: handle,
         description: listing["description"],
         skills: Enum.map_join(skills, "\n", &(&1["name"] <> "\n" <> &1["description"])),
         tags: skills |> Enum.flat_map(& &1["tags"]) |> Enum.join(" ")
       }}
    else
      _ -> {:error, :invalid}
    end
  end

  defp skill?(%{} = skill) do
    Enum.sort(Map.keys(skill)) == @skill_keys and is_binary(skill["id"]) and
      Regex.match?(@skill_id, skill["id"]) and text?(skill["name"], @max_skill_name) and
      text?(skill["description"], @max_skill_description) and tags?(skill["tags"])
  end

  defp skill?(_skill), do: false

  defp tags?(tags) when is_list(tags) and tags != [] and length(tags) <= @max_tags,
    do: Enum.all?(tags, &(is_binary(&1) and Regex.match?(@tag, &1))) and unique?(tags)

  defp tags?(_tags), do: false

  defp unique?(values), do: length(Enum.uniq(values)) == length(values)

  defp text?(value, max) when is_binary(value) do
    String.valid?(value) and String.trim(value) != "" and
      length(String.codepoints(value)) <= max and not Regex.match?(@hidden, value)
  end

  defp text?(_value, _max), do: false

  defp index(id, fields) do
    Repo.query!("DELETE FROM listing_search WHERE rowid = ?1", [id])

    Repo.query!(
      "INSERT INTO listing_search(rowid, name, description, skills, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
      [id, fields.name, fields.description, fields.skills, fields.tags]
    )
  end

  @doc "The listing of `endpoint`, as it sent it."
  def get(endpoint, now) do
    with {:ok, _} <- fetch_account(endpoint) do
      case Repo.one(from l in Listing, where: l.endpoint == ^endpoint and l.expires_at > ^now) do
        nil -> {:error, :not_found}
        listing -> {:ok, listing_json(listing)}
      end
    end
  end

  @doc "Whether `endpoint` has a listing at Unix time `now`."
  def listed?(endpoint, now) do
    Repo.exists?(from l in Listing, where: l.endpoint == ^endpoint and l.expires_at > ^now)
  end

  @doc """
  Deletes the listing of `endpoint` and its search row. Runs in the
  transaction of the caller when there is one.
  """
  def delete(endpoint) do
    Repo.transact(fn ->
      case Repo.get_by(Listing, endpoint: endpoint) do
        nil ->
          :ok

        listing ->
          Repo.query!("DELETE FROM listing_search WHERE rowid = ?1", [listing.id])
          Repo.delete!(listing)
      end

      {:ok, :ok}
    end)

    :ok
  end

  @doc "Deletes every listing that expired at Unix time `now`."
  def delete_expired(now \\ System.system_time(:second)) do
    Repo.transact(fn ->
      ids = Repo.all(from l in Listing, where: l.expires_at <= ^now, select: l.id)

      for chunk <- Enum.chunk_every(ids, 500) do
        Repo.query!(
          "DELETE FROM listing_search WHERE rowid IN (SELECT value FROM json_each(?1))",
          [JSON.encode!(chunk)]
        )

        Repo.delete_all(from l in Listing, where: l.id in ^chunk)
      end

      {:ok, :ok}
    end)

    :ok
  end

  ## Search

  @doc """
  The words and the `limit` of a search request. `q` has at most 128
  characters. Its words are the runs of letters and digits with 2 or more
  characters, and it needs 1 to 8 of them. `limit` is 1 to 20 (default 10).
  """
  def parse_search(params) do
    with q when is_binary(q) <- params["q"],
         true <- String.valid?(q) and length(String.codepoints(q)) <= @max_query,
         words = Regex.scan(~r/[\p{L}\p{N}]{2,}/u, q) |> List.flatten(),
         true <- length(words) in 1..@max_tokens,
         {:ok, limit} <- parse_limit(params["limit"]) do
      {:ok, words, limit}
    else
      _ -> {:error, :invalid}
    end
  end

  defp parse_limit(nil), do: {:ok, @default_limit}

  defp parse_limit(value) when is_binary(value) do
    case Integer.parse(value) do
      {limit, ""} when limit in 1..@max_limit -> {:ok, limit}
      _ -> :error
    end
  end

  defp parse_limit(_value), do: :error

  @doc """
  The listings that match every word in `words` as a prefix, as seen by
  `asker` at Unix time `now`. The best `limit` matches by rank, then by the
  last update; inside that page, the agents that are online come first.
  """
  def search(asker, words, limit, now) do
    with {:ok, _} <- fetch_account(asker) do
      # Each word is a quoted string, so FTS5 reads no operator in it.
      match = Enum.map_join(words, " ", &(~s(") <> &1 <> ~s("*)))

      %{rows: rows} =
        Repo.query!(
          """
          SELECT l.endpoint, l.listing, l.signature, l.expires_at, a.name, a.number
          FROM listing_search
          JOIN listings l ON l.id = listing_search.rowid
          JOIN accounts a ON a.endpoint = l.endpoint
          WHERE listing_search MATCH ?1 AND l.expires_at > ?2
          ORDER BY bm25(listing_search), l.updated_at DESC
          LIMIT ?3
          """,
          [match, now, limit]
        )

      contacts = contacts(asker, Enum.map(rows, &hd/1))

      agents =
        rows
        |> Enum.map(fn [endpoint, text, signature, expires_at, name, number] ->
          row(endpoint, Names.format(name, number), text, signature, expires_at, contacts, now)
        end)
        |> Enum.sort_by(&(not &1["online"]))

      {:ok, %{"agents" => agents}}
    end
  end

  @doc "The listed service agent `name#number`, as seen by `asker` at Unix time `now`."
  def lookup(asker, name, number, now) do
    with {:ok, _} <- fetch_account(asker) do
      result =
        Repo.one(
          from l in Listing,
            join: a in Account,
            on: a.endpoint == l.endpoint,
            where: a.name == ^name and a.number == ^number and l.expires_at > ^now,
            select: {l, a}
        )

      case result do
        nil ->
          {:error, :not_found}

        {listing, account} ->
          contacts = contacts(asker, [listing.endpoint])

          {:ok,
           row(
             listing.endpoint,
             full_name(account),
             listing.listing,
             listing.signature,
             listing.expires_at,
             contacts,
             now
           )}
      end
    end
  end

  defp row(endpoint, handle, text, signature, expires_at, contacts, now) do
    contact = MapSet.member?(contacts, endpoint)

    row = %{
      "handle" => handle,
      "listing" => text,
      "signature" => signature,
      "expiresAt" => expires_at,
      "online" => Presence.online?(endpoint, now),
      "contact" => contact,
      # The full name of the owner only: never the owner's presence or
      # endpoint.
      "owner" => AetherHub.OwnerLinks.active_owner(endpoint, now)
    }

    if contact, do: Map.put(row, "endpoint", endpoint), else: row
  end

  defp contacts(asker, endpoints) do
    from(c in Contact,
      where: c.owner_endpoint == ^asker and c.contact_endpoint in ^endpoints,
      select: c.contact_endpoint
    )
    |> Repo.all()
    |> MapSet.new()
  end

  defp listing_json(%Listing{} = listing) do
    %{
      "listed" => true,
      "listing" => listing.listing,
      "signature" => listing.signature,
      "expiresAt" => listing.expires_at
    }
  end

  defp full_name(%Account{name: name, number: number}), do: Names.format(name, number)

  defp fetch_account(endpoint) do
    case Repo.get(Account, endpoint) do
      nil -> {:error, :no_account}
      account -> {:ok, account}
    end
  end
end
