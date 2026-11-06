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

  alias AetherHub.{Account, Clock, Contacts, Json, Listing, Names, OwnerLinks, Params}
  alias AetherHub.{Presence, Protocol, Repo, Signed}

  @max_seconds Protocol.limit(:listing_max_days) * 86_400
  @max_bytes Protocol.limit(:listing_max_bytes)
  @body_keys ["expiresAt", "listed", "listing", "signature"]
  @listing_keys ["description", "name", "skills"]
  @skill_keys ["description", "id", "name", "tags"]
  @max_description Protocol.limit(:listing_max_description)
  @max_skill_name Protocol.limit(:listing_max_skill_name)
  @max_skill_description Protocol.limit(:listing_max_skill_description)
  @max_skills Protocol.limit(:listing_max_skills)
  @max_tags Protocol.limit(:listing_max_tags)
  @hidden Protocol.limit(:listing_hidden_codepoints)
  @max_query Protocol.limit(:agents_max_query_chars)
  @max_words Protocol.limit(:agents_max_words)
  @min_word Protocol.limit(:agents_min_word_chars)
  @service Protocol.kind(:service)

  ## The listing of this node

  @doc """
  Keeps the listing in `body` for the service agent `account`, and updates
  the search index in the same transaction.
  """
  def put(%Account{kind: @service, endpoint: endpoint} = account, body) do
    with {:ok, text, bytes, signature, expires_at} <- signed_body(endpoint, body),
         {:ok, fields} <- parse(bytes, Account.full_name(account)) do
      Repo.atomic(fn ->
        listing =
          (Repo.get_by(Listing, endpoint: endpoint) || %Listing{endpoint: endpoint})
          |> Ecto.Changeset.change(
            listing: text,
            signature: signature,
            expires_at: expires_at,
            updated_at: Clock.now()
          )
          |> Repo.insert_or_update!()

        unindex([listing.id])
        index(listing.id, fields)
        json(listing)
      end)
    end
  end

  def put(%Account{}, _body), do: {:error, :invalid}

  @doc """
  Whether `bytes` are a listing JSON object that the hub would accept for
  `handle`, before signing or storage.
  """
  def valid_listing?(bytes, handle) when is_binary(bytes) and is_binary(handle) do
    match?({:ok, _}, parse(bytes, handle))
  end

  defp signed_body(endpoint, %{} = body) do
    now = Clock.now()

    with true <- Enum.sort(Map.keys(body)) == @body_keys,
         true <- body["listed"] == true,
         expires_at when is_integer(expires_at) <- body["expiresAt"],
         true <- expires_at > now and expires_at <= now + @max_seconds,
         text when is_binary(text) <- body["listing"],
         {:ok, bytes} <- Base.url_decode64(text, padding: false),
         true <- byte_size(bytes) <= @max_bytes,
         signature when is_binary(signature) <- body["signature"],
         true <-
           Signed.valid?(endpoint, Signed.listing_message(endpoint, expires_at, bytes), signature) do
      {:ok, text, bytes, signature, expires_at}
    else
      _ -> {:error, :invalid}
    end
  end

  defp signed_body(_endpoint, _body), do: {:error, :invalid}

  defp parse(bytes, handle) do
    with {:ok, %{} = listing} <- Json.decode(bytes),
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
      Regex.match?(Protocol.regex(:skill_id), skill["id"]) and
      text?(skill["name"], @max_skill_name) and
      text?(skill["description"], @max_skill_description) and tags?(skill["tags"])
  end

  defp skill?(_skill), do: false

  defp tags?(tags) when is_list(tags) and tags != [] and length(tags) <= @max_tags,
    do:
      Enum.all?(tags, &(is_binary(&1) and Regex.match?(Protocol.regex(:tag), &1))) and
        unique?(tags)

  defp tags?(_tags), do: false

  defp unique?(values), do: length(Enum.uniq(values)) == length(values)

  # Control characters, and characters with no width or that change the
  # direction of the text, are refused.
  defp text?(value, max) when is_binary(value) do
    String.valid?(value) and String.trim(value) != "" and
      length(String.codepoints(value)) <= max and
      not Enum.any?(String.to_charlist(value), fn char -> Enum.any?(@hidden, &(char in &1)) end)
  end

  defp text?(_value, _max), do: false

  @doc """
  The description and the skill names and tags of a kept listing text, for
  the public directory. `:error` for a row that does not read.
  """
  def card(text) do
    with {:ok, bytes} <- Base.url_decode64(text, padding: false),
         {:ok, %{"description" => description, "skills" => skills}} <- Json.decode(bytes) do
      {:ok,
       %{
         "description" => description,
         "skills" => Enum.map(skills, &Map.take(&1, ["name", "tags"]))
       }}
    else
      _ -> :error
    end
  end

  ## The search index

  defp index(id, fields) do
    Repo.query!(
      "INSERT INTO listing_search(rowid, name, description, skills, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
      [id, fields.name, fields.description, fields.skills, fields.tags]
    )
  end

  defp unindex(ids) do
    for chunk <- Enum.chunk_every(ids, 500) do
      Repo.query!(
        "DELETE FROM listing_search WHERE rowid IN (SELECT value FROM json_each(?1))",
        [JSON.encode!(chunk)]
      )
    end

    :ok
  end

  ## Reading and ending listings

  @doc "The listing of `account`, as it sent it."
  def get(%Account{endpoint: endpoint}) do
    case Repo.one(from l in live(), where: l.endpoint == ^endpoint) do
      nil -> {:error, :not_found}
      listing -> {:ok, json(listing)}
    end
  end

  def listed?(endpoint), do: Repo.exists?(from l in live(), where: l.endpoint == ^endpoint)

  @doc """
  Deletes the listing of `endpoint` and its search row. Runs in the
  transaction of the caller when there is one.
  """
  def delete(endpoint) do
    Repo.atomic(fn ->
      case Repo.get_by(Listing, endpoint: endpoint) do
        nil ->
          :ok

        listing ->
          unindex([listing.id])
          Repo.delete!(listing)
          :ok
      end
    end)
    |> case do
      {:ok, :ok} -> :ok
      error -> error
    end
  end

  def delete_expired do
    Repo.atomic(fn ->
      ids = Repo.all(from l in Listing, where: l.expires_at <= ^Clock.now(), select: l.id)
      unindex(ids)

      for chunk <- Enum.chunk_every(ids, 500),
          do: Repo.delete_all(from l in Listing, where: l.id in ^chunk)

      :ok
    end)
    |> case do
      {:ok, :ok} -> :ok
      error -> error
    end
  end

  defp live, do: from(l in Listing, where: l.expires_at > ^Clock.now())

  ## Search

  @doc """
  The words and the `limit` of a search request. Its words are the runs of
  letters and digits in `q`.
  """
  def parse_search(params) do
    with q when is_binary(q) <- params["q"],
         true <- String.valid?(q) and length(String.codepoints(q)) <= @max_query,
         words = Regex.scan(~r/[\p{L}\p{N}]{#{@min_word},}/u, q) |> List.flatten(),
         true <- length(words) in 1..@max_words,
         {:ok, limit} <- Params.limit(params["limit"]) do
      {:ok, words, limit}
    else
      _ -> {:error, :invalid}
    end
  end

  @doc """
  The listings that match every word in `words` as a prefix, as seen by
  `asker`. The best `limit` matches by rank, then by the last update;
  inside that page, the agents that are online come first.
  """
  def search(%Account{endpoint: asker}, words, limit) do
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
        [match, Clock.now(), limit]
      )

    contacts = Contacts.known(asker, Enum.map(rows, &hd/1))

    agents =
      rows
      |> Enum.map(fn [endpoint, text, signature, expires_at, name, number] ->
        listing = %Listing{
          endpoint: endpoint,
          listing: text,
          signature: signature,
          expires_at: expires_at
        }

        row(listing, Names.format(name, number), contacts)
      end)
      |> Enum.sort_by(&(not &1["online"]))

    {:ok, %{"agents" => agents}}
  end

  @doc "The listed service agent `name#number`, as seen by `asker`."
  def lookup(%Account{endpoint: asker}, name, number) do
    result =
      Repo.one(
        from l in live(),
          join: a in Account,
          on: a.endpoint == l.endpoint,
          where: a.name == ^name and a.number == ^number,
          select: {l, a}
      )

    case result do
      nil ->
        {:error, :not_found}

      {listing, account} ->
        {:ok, row(listing, Account.full_name(account), Contacts.known(asker, [listing.endpoint]))}
    end
  end

  defp row(%Listing{endpoint: endpoint} = listing, handle, contacts) do
    contact = MapSet.member?(contacts, endpoint)

    row = %{
      "handle" => handle,
      "listing" => listing.listing,
      "signature" => listing.signature,
      "expiresAt" => listing.expires_at,
      "online" => Presence.online?(endpoint),
      "contact" => contact,
      # The full name of the owner only: never the owner's presence or
      # endpoint.
      "owner" => OwnerLinks.active_owner(endpoint)
    }

    if contact, do: Map.put(row, "endpoint", endpoint), else: row
  end

  defp json(%Listing{} = listing) do
    %{
      "listed" => true,
      "listing" => listing.listing,
      "signature" => listing.signature,
      "expiresAt" => listing.expires_at
    }
  end
end
