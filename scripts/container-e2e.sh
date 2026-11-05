#!/bin/sh
set -eu

image="${1:-aether-e2e:local}"
network="aether-e2e-$$"
server="aether-e2e-server-$$"
state="aether-e2e-state-$$"

cleanup() {
  docker rm -f "$server" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  docker volume rm "$state" >/dev/null 2>&1 || true
}

trap cleanup EXIT INT TERM
docker network create "$network" >/dev/null
docker volume create "$state" >/dev/null
docker run --rm --mount "source=$state,target=/state" "$image" node fixture --data-dir /state >/dev/null
docker run -d --name "$server" --network "$network" --mount "source=$state,target=/state" -e AETHER_CODEX_BIN=/state/fake-codex "$image" node serve --policy /state/policy.json --data-dir /state/server --port 4242 >/dev/null

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
docker run --rm --mount "source=$state,target=/state" --entrypoint sh "$image" -c 'aether node invite --data-dir /state/server --agent-id server --expires-in 600 > /state/direct-invitation.json'
docker run --rm --network "$network" --mount "source=$state,target=/state" "$image" node direct --data-dir /state/client import /state/direct-invitation.json >/dev/null
result="$(docker run --rm --network "$network" --mount "source=$state,target=/state" "$image" node request --data-dir /state/client --agent-id server --address "$server:4242" --scope direct --text review --authorization /state/authorization.json)"
printf '%s\n' "$result"
printf '%s\n' "$result" | grep -F '"text":"fake Codex result"' >/dev/null
docker stop "$server" >/dev/null
