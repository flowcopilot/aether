# Shared steps of the container end-to-end scripts and the demo scripts.
# Source it: . "$(dirname "$0")/lib.sh"

# The answer of the fake Codex that `aether node fixture` writes
# (crates/aether/src/fixture.rs).
FIXTURE_ANSWER="fake Codex result"

# The containers, networks and volumes that `cleanup` removes.
containers=""
networks=""
volumes=""

cleanup() {
  for name in $containers; do docker rm -f "$name" >/dev/null 2>&1 || true; done
  for name in $networks; do docker network rm "$name" >/dev/null 2>&1 || true; done
  for name in $volumes; do docker volume rm "$name" >/dev/null 2>&1 || true; done
}

# new_network <name>, new_volume <name>, and run_detached <name> <docker run
# arguments>: each creates a resource that `cleanup` removes.
new_network() {
  docker network create "$1" >/dev/null
  networks="$networks $1"
}

new_volume() {
  docker volume create "$1" >/dev/null
  volumes="$volumes $1"
}

run_detached() {
  name="$1"
  shift
  containers="$containers $name"
  docker run -d --name "$name" "$@" >/dev/null
}

# retry <tries> <seconds> <command> [<argument>...]: runs the command until
# it succeeds, at most <tries> times, with <seconds> between the tries.
retry() {
  tries="$1"
  pause="$2"
  shift 2
  attempt=1
  until "$@"; do
    [ "$attempt" -lt "$tries" ] || return 1
    attempt=$((attempt + 1))
    sleep "$pause"
  done
}

logs_contain() {
  docker logs "$1" 2>&1 | grep -qF "$2"
}

# wait_for_log <container> <text>: waits up to 20 s for the text in the
# container's logs, and prints the logs when it does not come.
wait_for_log() {
  retry 20 1 logs_contain "$1" "$2" || {
    docker logs "$1" 2>&1
    return 1
  }
}

# The endpoint ID in the node log lines on stdin: the last one.
endpoint_id() {
  sed -n 's/.*"endpoint_id":"\([^"]*\)".*/\1/p' | tail -n 1
}

# container_endpoint_id <container>: the endpoint ID that the node in the
# container printed, or nothing yet.
container_endpoint_id() {
  docker logs "$1" 2>&1 | endpoint_id
}

# invite_direct <image> <state volume> <network>: the node in /state/server
# invites the node in /state/client, and the client imports the invitation.
invite_direct() {
  docker run --rm --mount "source=$2,target=/state" --entrypoint sh "$1" \
    -c 'aether node invite --data-dir /state/server --agent-id server --expires-in 600 > /state/direct-invitation.json'
  docker run --rm --network "$3" --mount "source=$2,target=/state" "$1" \
    node direct --data-dir /state/client import /state/direct-invitation.json >/dev/null
}

# has_answer <result> <text>: the task result holds the answer text.
has_answer() {
  printf '%s\n' "$1" | grep -F "\"text\":\"$2\"" >/dev/null
}

# caller_tools <aether image>: the names of the tools that
# `aether mcp serve --caller-only` lists, one on each line.
caller_tools() {
  printf '%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
    | docker run --rm -i "$1" mcp serve --caller-only --data-dir /tmp/caller-tools \
    | grep '"id":2' | grep -oE '"name":"aether\.[a-z_.]+"' | sed 's/^"name":"//; s/"$//'
}
