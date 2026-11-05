#!/bin/sh
set -eu
. "$(dirname "$0")/lib.sh"

image="${1:-aether-e2e:local}"
network="aether-e2e-$$"
server="aether-e2e-server-$$"
state="aether-e2e-state-$$"

trap cleanup EXIT INT TERM
new_network "$network"
new_volume "$state"
docker run --rm --mount "source=$state,target=/state" "$image" node fixture --data-dir /state >/dev/null
run_detached "$server" --network "$network" --mount "source=$state,target=/state" -e AETHER_CODEX_BIN=/state/fake-codex "$image" node serve --policy /state/policy.json --data-dir /state/server --port 4242
wait_for_log "$server" '"endpoint_id"'

invite_direct "$image" "$state" "$network"
result="$(docker run --rm --network "$network" --mount "source=$state,target=/state" "$image" node request --data-dir /state/client --agent-id server --address "$server:4242" --scope direct --text review --authorization /state/authorization.json)"
printf '%s\n' "$result"
has_answer "$result" "$FIXTURE_ANSWER"
docker stop "$server" >/dev/null
