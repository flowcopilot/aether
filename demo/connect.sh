#!/bin/sh
# Alice and Bob invite each other, as on the admin page: each node grants
# the other one access and gives it a connect code, and the other node
# imports the code. Run it after `docker compose -f demo/compose.yml up -d`.
# It prints the agent names to use with aether.task.send.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../scripts/lib.sh"
compose="docker compose -f $here/compose.yml"
call="python3 $here/mcp-call.py"

# node_id <service> <node log>: sets id to the endpoint ID that the node
# printed, and fails while it has printed none.
node_id() {
  id="$($compose exec -T "$1" cat "$2" 2>/dev/null | endpoint_id)"
  [ -n "$id" ]
}

# The endpoint ID that a node prints when it listens.
endpoint() {
  retry 60 1 node_id "$1" "$2" || { echo "connect: $1's node did not start" >&2; exit 1; }
  printf '%s\n' "$id"
}

# invite <host> <guest> <guest endpoint ID>: the host grants the guest access
# and the guest imports the code. Prints the host's agent name on the guest.
invite() {
  code="$($call "$1" aether.invites.code_create \
    "{\"friend_name\":\"$2\",\"endpoint_id\":\"$3\",\"days\":30}" \
    | sed -n 's/.*"code":"\([^"]*\)".*/\1/p')"
  [ -n "$code" ] || { echo "connect: $1 made no code for $2" >&2; exit 1; }
  $call "$2" aether.invites.code_import "{\"code\":\"$code\"}" \
    | sed -n 's/.*"agent":"\([^"]*\)".*/\1/p'
}

alice_id="$(endpoint alice /opt/data/logs/demo/node.log)"
bob_id="$(endpoint bob /root/.openclaw/logs/demo/node.log)"
alice_on_bob="$(invite alice bob "$bob_id")"
bob_on_alice="$(invite bob alice "$alice_id")"
[ -n "$alice_on_bob" ] && [ -n "$bob_on_alice" ] || { echo "connect: an import failed" >&2; exit 1; }
echo "ALICE_AGENT=$alice_on_bob"
echo "BOB_AGENT=$bob_on_alice"
