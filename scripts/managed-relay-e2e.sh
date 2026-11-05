#!/bin/sh
set -eu
. "$(dirname "$0")/lib.sh"

node_image="${1:-aether-e2e:local}"
relay_image="${2:-iroh-relay:local}"
server_network="aether-managed-server-network-$$"
client_network="aether-managed-client-network-$$"
relay="aether-managed-relay-$$"
control="aether-managed-control-$$"
server="aether-managed-server-$$"
state="aether-managed-state-$$"
relay_url="http://relay:3340"
control_url="https://control:8443"
certificate="/state/.aether/control/control-cert.der"

invite() {
  invitation="$(docker run --rm --mount "source=$state,target=/state" --workdir /state "$node_image" control invite --scope managed --expires-in 600)"
  token="$(printf '%s\n' "$invitation" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
  test -n "$token"
  printf '%s\n' "$token"
}

save_token() {
  token_path="$1"
  invite | docker run --rm -i --mount "source=$state,target=/state" --entrypoint sh "$node_image" -c "umask 077; cat > $token_path"
}

trap cleanup EXIT INT TERM
new_network "$server_network"
new_network "$client_network"
new_volume "$state"
run_detached "$relay" --network "$server_network" --network-alias relay -e RUST_LOG=info "$relay_image" --dev
docker network connect --alias relay "$client_network" "$relay"
wait_for_log "$relay" 'relay: serving on'

run_detached "$control" --network "$server_network" --network-alias control --mount "source=$state,target=/state" --workdir /state "$node_image" control --port 8443
docker network connect --alias control "$client_network" "$control"
wait_for_log "$control" 'control_listening'

docker run --rm --mount "source=$state,target=/state" "$node_image" node fixture --data-dir /state --scope managed >/dev/null
docker run --rm --mount "source=$state,target=/state" --entrypoint sh "$node_image" -c 'aether node record --data-dir /state/client --agent-id fixture-caller --address 127.0.0.1:4242 --scope managed --capability review --expires-in 600 > /state/client-record.json'
docker run --rm --mount "source=$state,target=/state" --entrypoint sh "$node_image" -c 'aether node record --data-dir /state/server --agent-id server --address 127.0.0.1:4242 --scope managed --capability review --expires-in 600 > /state/server-record.json'

save_token /state/client.token
docker run --rm --network "$client_network" --mount "source=$state,target=/state" "$node_image" node managed --control "$control_url" --control-certificate "$certificate" redeem --token-file /state/client.token /state/client-record.json >/dev/null
save_token /state/server.token
docker run --rm --network "$server_network" --mount "source=$state,target=/state" "$node_image" node managed --control "$control_url" --control-certificate "$certificate" redeem --token-file /state/server.token /state/server-record.json >/dev/null

run_detached "$server" --network "$server_network" --mount "source=$state,target=/state" -e AETHER_RELAY_URL="$relay_url" -e AETHER_CODEX_BIN=/state/fake-codex "$node_image" node serve --policy /state/policy.json --data-dir /state/server --port 4242 --managed-control "$control_url" --managed-control-certificate "$certificate"
wait_for_log "$server" 'node_listening'

result="$(docker run --rm --network "$client_network" --mount "source=$state,target=/state" -e AETHER_RELAY_URL="$relay_url" "$node_image" node request --data-dir /state/client --managed-control "$control_url" --managed-control-certificate "$certificate" --agent-id server --scope managed --text review --authorization /state/authorization.json)"
printf '%s\n' "$result"
has_answer "$result" "$FIXTURE_ANSWER"
docker stop "$server" >/dev/null
