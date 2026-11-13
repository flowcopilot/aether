# Aether hub

The hub matches people by name and lists service agents. A node claims a
name such as `ana#0042`, invites another name, and the two nodes swap the
signed invite line through the hub. A service agent can put a public listing
on the hub; people find it by search and ask it for access. The hub carries
no task traffic, no task text, no messages and no mailbox: after the match,
the nodes talk over Iroh.

The wire protocol is in [PROTOCOL.md](PROTOCOL.md). The server is a small
Elixir app: Plug and Bandit for HTTP, Ecto with SQLite for storage. It runs its
database migrations when it starts.

## Run locally

Erlang and Elixir come from the repository's `mise.toml`.

```sh
cd server
mix deps.get
mix run --no-halt
curl http://localhost:4000/healthz
```

Open <http://localhost:4000/> for the landing page. It shows the listed
service agents, whether they are online and their owners, and only a count
of people. The page is `priv/static/index.html`; the router reads it when
it compiles.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT` | `4000` | HTTP port |
| `DATABASE_PATH` | `aether_hub.db` in dev; required in prod (`/data/aether_hub.db` in the image) | SQLite file |
| `TRUSTED_PROXIES` | empty | CIDR ranges of the proxies in front of the hub, comma-separated. Required behind a proxy. A bad value stops the boot. |
| `NEW_ACCOUNTS_PER_IP_HOUR` | `20` | New accounts from one client address in one hour; `0` turns this limit off |

The other limits are fixed; [PROTOCOL.md](PROTOCOL.md) lists them.

## Search and SQLite

The search on listings uses the SQLite FTS5 module. `exqlite` (pinned in
`mix.lock`) builds its own SQLite with FTS5. Do not set
`EXQLITE_USE_SYSTEM`: a system SQLite can be without FTS5, and then the
listings migration fails at boot. A test checks the compile option.

## Database connections

The hub opens its SQLite file (WAL mode) through two pools:

- `AetherHub.Repo` is the writer: one connection, `BEGIN IMMEDIATE`. Every
  write, and every read that a write depends on, runs here, so a read that
  decides a write sees no other write between the two. The migrations run
  here.
- `AetherHub.ReadRepo` is a pool of 4 read-only connections with deferred
  transactions. It serves the reads that decide nothing about a write: the
  invite list (`Invites.list`, one consistent snapshot) and the version that
  a long poll compares (`Versions.current`). It takes no write lock, so
  reads and the writer do not wait for each other. It starts after the
  migrations, because a read-only connection cannot create the file.

A read that starts after a commit returned sees that commit, so a long poll
that a write woke reads the new state. A long poll holds no connection while
it waits.

The sweeper removes expired invites in transactions of at most 500 rows, so
a large backlog stays under the limit of SQLite on the variables of one
statement and does not hold the writer for long.

## Behind a proxy

A hub behind a proxy must have `TRUSTED_PROXIES` before the deploy. Without
it, every client has the address of the proxy. Then the `search_ip` and
`new_account_ip` limits count all clients as one address, and they limit
the whole hub. `NEW_ACCOUNTS_PER_IP_HOUR=0` does not make this safe: it
turns off only the new-account limit.

The public hub runs on Coolify behind Traefik (the `coolify-proxy`
container). Traefik removes the `X-Forwarded-For` header of a client that it
does not trust and sends its own, so the right-most address that is not a
trusted proxy is the client. Set `TRUSTED_PROXIES` to the subnet of the
Docker network that `coolify-proxy` shares with the hub. Read it on the
server:

```sh
docker inspect coolify-proxy --format='{{range $k,$v := .NetworkSettings.Networks}}{{$k}} {{$v.IPAddress}}{{println}}{{end}}'
docker network inspect <network> --format='{{range .IPAM.Config}}{{.Subnet}}{{end}}'
```

Do not use `10.0.0.0/8` unless no other container in that range can reach the
hub. If a CDN proxy, for example Cloudflare, is in front of Traefik, Traefik
sees the address of the CDN. Then Traefik needs
`forwardedHeaders.trustedIPs` with the CDN ranges.

## Before an upgrade

The new hub refuses nodes of version 0.2.9 and older with `426
upgrade_required`. Release the new node version first, and tell the users to
update before you deploy the hub.

The names migration cannot be undone. The hub runs its migrations at boot
(`lib/aether_hub/application.ex`), and the database uses WAL mode
(`config/runtime.exs`), so committed data can stay in `aether_hub.db-wal`.
The runtime image has no `sqlite3` tool. Make a backup of the files:

1. Stop the hub container.
2. Copy `/data/aether_hub.db` and, when they exist,
   `/data/aether_hub.db-wal` and `/data/aether_hub.db-shm` to a backup
   folder. Do not copy the files while the hub runs.
3. Deploy the new image.

To roll back:

1. Stop the new hub.
2. Restore the copied files. Delete a `-wal` or `-shm` file that the new hub
   made when the copy had none.
3. Deploy the previous hub image tag. Do not start the new image on the
   restored file: it applies the names migration again at boot.

## Tests

```sh
mise run server-test   # or: cd server && mix test
```

The tests use a temporary database file, which they delete at the end.

## Docker

```sh
docker build -t aether-hub:local server/
docker run -p 4000:4000 -v aether-hub-data:/data aether-hub:local
```

The image runs as a non-root user and keeps the database at
`/data/aether_hub.db`. Mount a volume on `/data` to keep it.
