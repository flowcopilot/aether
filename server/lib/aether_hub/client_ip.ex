defmodule AetherHub.ClientIp do
  @moduledoc """
  Finds the client address of a request and assigns it as `:client_ip`.

  When the peer is a trusted proxy (`TRUSTED_PROXIES`), the plug reads
  `x-forwarded-for` from right to left, skips trusted and invalid entries,
  and takes the first other address. A proxy adds its client to the right
  end, and a client can write any value on the left, so the left-most value
  is never taken for its position alone. Else the peer is the client.
  """
  @behaviour Plug

  import Bitwise
  import Plug.Conn

  @key {__MODULE__, :trusted}

  @impl true
  def init(opts), do: opts

  @impl true
  def call(conn, _opts) do
    forwarded = get_req_header(conn, "x-forwarded-for")
    assign(conn, :client_ip, client_ip(conn.remote_ip, forwarded, trusted()))
  end

  @doc "Keeps the trusted proxy ranges for every request."
  def put_trusted(cidrs), do: :persistent_term.put(@key, cidrs)

  defp trusted, do: :persistent_term.get(@key, [])

  @doc """
  The client of a request from `peer` with the `x-forwarded-for` header
  values `forwarded`, behind the proxies in `trusted`.
  """
  def client_ip(peer, forwarded, trusted) do
    if trusted?(peer, trusted) do
      forwarded
      |> Enum.flat_map(&String.split(&1, ","))
      |> Enum.map(&String.trim/1)
      |> Enum.reverse()
      |> Enum.find_value(peer, fn value ->
        case :inet.parse_strict_address(String.to_charlist(value)) do
          {:ok, ip} -> if trusted?(ip, trusted), do: nil, else: ip
          {:error, _} -> nil
        end
      end)
    else
      peer
    end
  end

  @doc """
  Parses a comma-separated list of CIDR ranges, such as
  `"10.0.1.0/24, fd00::/8"`. An address without a prefix is one host. Raises
  `ArgumentError` for a bad value, so a bad setting stops the boot.
  """
  def parse_cidrs!(value) when is_binary(value) do
    value
    |> String.split(",", trim: true)
    |> Enum.map(&String.trim/1)
    |> Enum.reject(&(&1 == ""))
    |> Enum.map(&parse_cidr!/1)
  end

  defp parse_cidr!(text) do
    {address, prefix} =
      case String.split(text, "/") do
        [address] -> {address, nil}
        [address, prefix] -> {address, prefix}
        _ -> raise ArgumentError, "bad CIDR range: #{text}"
      end

    ip =
      case :inet.parse_strict_address(String.to_charlist(address)) do
        {:ok, ip} -> ip
        {:error, _} -> raise ArgumentError, "bad CIDR range: #{text}"
      end

    bits = address_bits(ip)

    prefix =
      case prefix && Integer.parse(prefix) do
        nil -> bits
        {n, ""} when n >= 0 and n <= bits -> n
        _ -> raise ArgumentError, "bad CIDR range: #{text}"
      end

    {to_integer(ip), bits, prefix}
  end

  defp trusted?(ip, trusted) do
    value = to_integer(ip)
    bits = address_bits(ip)

    Enum.any?(trusted, fn {base, range_bits, prefix} ->
      range_bits == bits and value >>> (bits - prefix) == base >>> (bits - prefix)
    end)
  end

  defp address_bits(ip) when tuple_size(ip) == 4, do: 32
  defp address_bits(ip) when tuple_size(ip) == 8, do: 128

  defp to_integer({a, b, c, d}), do: (a <<< 24) + (b <<< 16) + (c <<< 8) + d

  defp to_integer(ip) when tuple_size(ip) == 8 do
    ip |> Tuple.to_list() |> Enum.reduce(0, fn part, acc -> (acc <<< 16) + part end)
  end
end
