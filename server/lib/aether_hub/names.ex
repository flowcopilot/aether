defmodule AetherHub.Names do
  @moduledoc """
  Hub names: `name#NNNN`. The name part is short and lowercase; the hub
  picks the four-digit number. A tag that a key released stays with that key.
  """

  import Ecto.Query

  alias AetherHub.{Account, ReleasedName, Repo}

  @name ~r/\A[a-z0-9][a-z0-9._-]{0,31}\z/
  @handle ~r/\A([a-z0-9][a-z0-9._-]{0,31})#([0-9]{4})\z/
  @numbers 1..9999

  @doc "Whether `name` can be the name part of a hub name."
  def valid_name?(name) when is_binary(name), do: Regex.match?(@name, name)
  def valid_name?(_name), do: false

  @doc "Splits a full name such as `ana#0042`. The number `0000` is refused."
  def parse_handle(handle) when is_binary(handle) do
    case Regex.run(@handle, handle) do
      [_, name, digits] ->
        case String.to_integer(digits) do
          0 -> :error
          number -> {:ok, {name, number}}
        end

      _ ->
        :error
    end
  end

  def parse_handle(_handle), do: :error

  @doc "The full name of `name` with `number`, such as `ana#0042`."
  def format(name, number),
    do: name <> "#" <> String.pad_leading(Integer.to_string(number), 4, "0")

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

  @doc "The numbers that `endpoint` released for `name`."
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
      [
        %{
          name: name,
          number: number,
          endpoint: endpoint,
          kind: kind,
          released_at: System.system_time(:second)
        }
      ],
      on_conflict: :nothing,
      conflict_target: [:name, :number]
    )

    :ok
  end
end
