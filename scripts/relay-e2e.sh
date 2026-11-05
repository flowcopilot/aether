#!/bin/sh
set -eu

node_image="${1:-aether-e2e:local}"
relay_image="${2:-iroh-relay:local}"
server_network="aether-e2e-server-network-$$"
client_network="aether-e2e-client-network-$$"
relay="aether-e2e-relay-$$"
server="aether-e2e-server-$$"
state="aether-e2e-state-$$"
relay_url="http://relay:3340"

cleanup() {
  docker rm -f "$server" >/dev/null 2>&1 || true
  docker rm -f "$relay" >/dev/null 2>&1 || true
  docker network rm "$server_network" >/dev/null 2>&1 || true
  docker network rm "$client_network" >/dev/null 2>&1 || true
  docker volume rm "$state" >/dev/null 2>&1 || true
}

trap cleanup EXIT INT TERM
docker network create "$server_network" >/dev/null
docker network create "$client_network" >/dev/null
docker volume create "$state" >/dev/null
docker run --rm --mount "source=$state,target=/state" "$node_image" node fixture --data-dir /state >/dev/null
docker run -d --name "$relay" --network "$server_network" --network-alias relay -e RUST_LOG=info "$relay_image" --dev >/dev/null
docker network connect --alias relay "$client_network" "$relay"

attempt=0
while [ "$attempt" -lt 20 ]; do
  if docker logs "$relay" 2>&1 | grep -q 'relay: serving on'; then
    break
  fi
  sleep 1
  attempt=$((attempt + 1))
done
docker logs "$relay" 2>&1 | grep -q 'relay: serving on'

docker run -d --name "$server" --network "$server_network" --mount "source=$state,target=/state" -e AETHER_RELAY_URL="$relay_url" -e AETHER_CODEX_BIN=/state/fake-codex "$node_image" node serve --policy /state/policy.json --data-dir /state/server --port 4242 >/dev/null

endpoint_id=""
attempt=0
while [ "$attempt" -lt 20 ]; do
  endpoint_id="$(docker logs "$server" 2>&1 | sed -n 's/.*"endpoint_id":"\([^"]*\)".*/\1/p')"
  if [ -n "$endpoint_id" ]; then
    break
  fi
  sleep 1
  attempt=$((attempt + 1))
done

test -n "$endpoint_id"
result="$(docker run --rm --network "$client_network" --mount "source=$state,target=/state" -e AETHER_RELAY_URL="$relay_url" "$node_image" node request --data-dir /state/client --endpoint-id "$endpoint_id" --address relay:3340 --scope direct --text review --authorization /state/authorization.json)"
printf '%s\n' "$result"
printf '%s\n' "$result" | grep -F '"text":"fake Codex result"' >/dev/null
docker stop "$server" >/dev/null
