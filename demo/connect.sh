#!/bin/sh
# Alice and Bob invite each other, as on the admin page: each node grants
# the other one access and gives it a connect code, and the other node
# imports the code. Run it after `docker compose -f demo/compose.yml up -d`.
# It prints the agent names to use with aether.task.send.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
compose="docker compose -f $here/compose.yml"
call="python3 $here/mcp-call.py"

# The endpoint ID that a node prints when it listens.
endpoint() {
  attempt=0
  until id="$($compose exec -T "$1" sh -c "cat $2" 2>/dev/null \
    | sed -n 's/.*"endpoint_id":"\([^"]*\)".*/\1/p' | tail -n 1)" && [ -n "$id" ]; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 60 ] || { echo "connect: $1's node did not start" >&2; exit 1; }
    sleep 1
  done
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
