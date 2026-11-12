defmodule AetherHub.Names do
  @moduledoc """
  Hub names: `name#NNNN`. The name part is short and lowercase; the hub
  picks the four-digit number. A tag that a key released stays with that key.
  """

  import Ecto.Query

  alias AetherHub.{Account, Clock, Protocol, ReleasedName, Repo}

  @numbers Protocol.limit(:numbers)

  def valid_name?(name) when is_binary(name), do: Regex.match?(Protocol.regex(:name), name)
  def valid_name?(_name), do: false

  @doc "Splits a full name such as `ana#0042`. The number `0000` is refused."
  def parse_handle(handle) when is_binary(handle) do
    with [name, digits] <- String.split(handle, "#"),
         true <- valid_name?(name) and digits =~ ~r/\A[0-9]{4}\z/,
         number when number in @numbers <- String.to_integer(digits) do
      {:ok, {name, number}}
    else
      _ -> :error
    end
  end

  def parse_handle(_handle), do: :error

  def format(name, number),
    do: name <> "#" <> String.pad_leading(Integer.to_string(number), 4, "0")

  @doc """
  The account with the full name `handle`: `{:error, :invalid}` for a bad
  full name, `{:error, :not_found}` when no account has it.
  """
  def find(handle) do
    with {:ok, {name, number}} <- parse_handle(handle),
         %Account{} = account <-
           Repo.one(from a in Account, as: :account, where: ^named(name, number)) do
      {:ok, account}
    else
      :error -> {:error, :invalid}
      nil -> {:error, :not_found}
    end
  end

  @doc """
  The account with the full name `handle` and the type `kind`. An account
  of another type gets the answer of an unknown name, so the answer does
  not show that it exists.
  """
  def find(handle, kind) do
    case find(handle) do
      {:ok, %Account{kind: ^kind}} = found -> found
      {:ok, %Account{}} -> {:error, :not_found}
      error -> error
    end
  end

  @doc "The query condition for the account `name#number`, bound as `:account`."
  def named(name, number), do: dynamic([account: a], a.name == ^name and a.number == ^number)

  @doc """
  A number for `name` that is free for `endpoint`: no other account holds
  it, and no other key released it. The first free number in `preferred`
  wins; else a random free number. `{:error, :taken}` when none is free.
  """
  def pick_number(name, endpoint, preferred) do
    held =
      from(a in Account, where: a.name == ^name and a.endpoint != ^endpoint, select: a.number)
      |> Repo.all()

    released =
      from(r in ReleasedName,
        where: r.name == ^name and r.endpoint != ^endpoint,
        select: r.number
      )
      |> Repo.all()

    taken = MapSet.new(held ++ released)

    case Enum.find(preferred, &(&1 in @numbers and not MapSet.member?(taken, &1))) do
      nil ->
        case Enum.reject(@numbers, &MapSet.member?(taken, &1)) do
          [] -> {:error, :taken}
          free -> {:ok, Enum.random(free)}
        end

      number ->
        {:ok, number}
    end
  end

  @doc "The numbers that `endpoint` released for `name`, the latest first."
  def released_numbers(name, endpoint) do
    from(r in ReleasedName,
      where: r.name == ^name and r.endpoint == ^endpoint,
      order_by: [desc: r.released_at],
      select: r.number
    )
    |> Repo.all()
  end

  @doc "Whether `endpoint` released a tag with another `kind`."
  def released_with_other_kind?(endpoint, kind) do
    Repo.exists?(from r in ReleasedName, where: r.endpoint == ^endpoint and r.kind != ^kind)
  end

  @doc "Keeps `name#number` for `endpoint`, with its `kind`."
  def release(name, number, endpoint, kind) do
    Repo.insert_all(
      ReleasedName,
      [%{name: name, number: number, endpoint: endpoint, kind: kind, released_at: Clock.now()}],
      on_conflict: :nothing,
      conflict_target: [:name, :number]
    )

    :ok
  end
end
