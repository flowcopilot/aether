defmodule AetherHub.Features.DirectoryTest do
  use AetherHub.Feature, file: "directory.feature", async: true

  alias AetherHub.{Clock, Contract}

  defp public(path, ip \\ unique_ip()), do: public_request(:get, path, ip)

  defp directory, do: public("/directory.json")

  defp row(handle), do: Enum.find(json(directory())["agents"], &(&1["handle"] == handle))

  defp content_type(conn), do: Plug.Conn.get_resp_header(conn, "content-type")

  # Public pages

  defstep ~r/^a visitor opens the landing page$/, _, _state do
    {:ok, %{conn: public("/")}}
  end

  defstep ~r/^a visitor opens the docs$/, _, _state do
    {:ok, %{conn: public("/docs")}}
  end

  defstep ~r/^the hub answers 200 with HTML$/, _, %{conn: conn} do
    assert conn.status == 200
    assert ["text/html" <> _] = content_type(conn)
  end

  defstep ~r/^the hub answers 200 with Markdown$/, _, %{conn: conn} do
    assert conn.status == 200
    assert ["text/markdown" <> _] = content_type(conn)
  end

  defstep ~r/^the page loads nothing by default and connects only to the hub$/, _, %{conn: conn} do
    assert [policy] = Plug.Conn.get_resp_header(conn, "content-security-policy")
    assert policy =~ "default-src 'none'"
    assert policy =~ "connect-src 'self'"
  end

  defstep ~r/^the page tells how to run "(?<command>[^"]+)"$/, %{command: command}, %{conn: conn} do
    assert conn.resp_body =~ command
  end

  # Rows

  defstep ~r/^(?<name>\w+) publishes the listing:$/, %{name: name, table: [values]}, state do
    service = actor(state, name)

    listing =
      listing(service,
        description: values["description"],
        skill: values["skill"],
        tags: String.split(values["tags"], ", ")
      )

    assert put_listing(service, listing).status == 200
  end

  defstep ~r/^the directory shows (?<name>\w+) with:$/, %{name: name, table: [values]}, state do
    handle = actor(state, name).handle

    assert row(handle) == %{
             "handle" => handle,
             "description" => values["description"],
             "skills" => [
               %{"name" => values["skill"], "tags" => String.split(values["tags"], ", ")}
             ],
             # The PUT of the listing was a signed request just now.
             "online" => values["online"] == "yes",
             "owner" => if(values["owner"] == "none", do: nil, else: values["owner"])
           }
  end

  defstep ~r/^(?<owner>\w+) owns (?<service>\w+)$/, %{owner: owner, service: service}, state do
    link_owner(actor(state, owner), actor(state, service))
  end

  defstep ~r/^the directory shows (?<owner>\w+) as the owner of (?<service>\w+)$/,
          %{owner: owner, service: service},
          state do
    assert row(actor(state, service).handle)["owner"] == actor(state, owner).handle
  end

  defstep ~r/^the row of (?<name>\w+) shows only the full name, the listing text, the skills, the owner and whether it is online$/,
          %{name: name},
          state do
    row = row(actor(state, name).handle)
    assert Enum.sort(Map.keys(row)) == Contract.keys("directoryAgent")
    assert Enum.all?(row["skills"], &(Enum.sort(Map.keys(&1)) == Contract.keys("directorySkill")))
  end

  defstep ~r/^the directory does not show the endpoint of (?<names>\w+(?: or \w+)*)$/,
          %{names: names},
          state do
    body = directory().resp_body

    for name <- String.split(names, " or ") do
      refute body =~ actor(state, name).endpoint
    end
  end

  defstep ~r/^the directory does not show (?<name>\w+)$/, %{name: name}, state do
    handle = actor(state, name).handle
    assert row(handle) == nil
    refute directory().resp_body =~ handle
  end

  # People

  defstep ~r/^the count of people in the directory is noted$/, _, _state do
    {:ok, %{people: json(directory())["people"]}}
  end

  defstep ~r/^a new person takes a name$/, _, _state do
    claimed_node()
  end

  defstep ~r/^the count of people in the directory is at least 1 higher$/, _, state do
    assert json(directory())["people"] >= state.people + 1
  end

  # The limit

  defstep ~r/^a visitor reads the directory as many times as the limit allows$/, _, _state do
    ip = unique_ip()
    limit = start_of_window(:directory_ip)

    {:ok, %{ip: ip, statuses: for(_ <- 1..limit, do: public("/directory.json", ip).status)}}
  end

  defstep ~r/^the visitor reads the directory once more$/, _, state do
    {:ok, %{conn: public("/directory.json", state.ip)}}
  end

  defstep ~r/^a visitor from another address reads the directory$/, _, _state do
    Clock.set(nil)
    {:ok, %{conn: directory()}}
  end
end
