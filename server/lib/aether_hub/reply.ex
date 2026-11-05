defmodule AetherHub.Reply do
  @moduledoc "Sends JSON answers and protocol errors."

  import Plug.Conn

  def json(conn, status, value) do
    conn |> put_resp_content_type("application/json") |> send_resp(status, JSON.encode!(value))
  end

  @doc "Sends the error `code` with its status from the contract."
  def error(conn, code),
    do: json(conn, AetherHub.Protocol.status(code), %{"error" => Atom.to_string(code)})
end
