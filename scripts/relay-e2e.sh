#!/bin/sh
set -eu
. "$(dirname "$0")/lib.sh"

node_image="${1:-aether-e2e:local}"
relay_image="${2:-iroh-relay:local}"
server_network="aether-e2e-server-network-$$"
client_network="aether-e2e-client-network-$$"
relay="aether-e2e-relay-$$"
server="aether-e2e-server-$$"
state="aether-e2e-state-$$"
relay_url="http://relay:3340"

trap cleanup EXIT INT TERM
new_volume "$state"
docker run --rm --mount "source=$state,target=/state" "$node_image" node fixture --data-dir /state >/dev/null
start_relay "$relay" "$relay_image" "$server_network" "$client_network"

run_detached "$server" --network "$server_network" --mount "source=$state,target=/state" -e AETHER_RELAY_URL="$relay_url" -e AETHER_CODEX_BIN=/state/fake-codex "$node_image" node serve --policy /state/policy.json --data-dir /state/server --port 4242
wait_for_log "$server" '"endpoint_id"'
endpoint_id="$(container_endpoint_id "$server")"

result="$(docker run --rm --network "$client_network" --mount "source=$state,target=/state" -e AETHER_RELAY_URL="$relay_url" "$node_image" node request --data-dir /state/client --endpoint-id "$endpoint_id" --address relay:3340 --scope direct --text review --authorization /state/authorization.json)"
printf '%s\n' "$result"
has_answer "$result" "$FIXTURE_ANSWER"
docker stop "$server" >/dev/null
