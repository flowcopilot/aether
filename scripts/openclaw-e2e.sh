#!/bin/sh
# One Aether task from a client node to a host node that answers with OpenClaw
# through the aether plugin. By default the model is a mock, so no provider key
# is needed. To use a real model through OpenRouter, set OPENROUTER_API_KEY and
# E2E_MODEL (for example openrouter/nvidia/nemotron-3.5-lightning:free); the key
# goes to the container as an
# environment variable only.
# Build the images first:
#   docker build -f Dockerfile.e2e -t aether-e2e:local .
#   docker build -f integrations/openclaw-plugin/e2e/Dockerfile \
#     -t aether-openclaw-e2e:local integrations/openclaw-plugin
set -eu

aether_image="${1:-aether-e2e:local}"
openclaw_image="${2:-aether-openclaw-e2e:local}"
network="aether-openclaw-e2e-$$"
host="aether-openclaw-e2e-host-$$"
state="aether-openclaw-e2e-state-$$"

logs() {
  docker ps -a --filter "name=$host" --format 'container: {{.Status}}'
  docker logs --tail 20 "$host" 2>&1 || true
  for name in node gateway model; do
    printf '\n--- %s.log ---\n' "$name"
    docker exec "$host" tail -n 60 "/state/logs/$name.log" 2>/dev/null || true
  done
}

cleanup() {
  docker rm -f "$host" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  docker volume rm "$state" >/dev/null 2>&1 || true
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  logs >&2
  exit 1
}

trap cleanup EXIT INT TERM
docker network create "$network" >/dev/null
docker volume create "$state" >/dev/null
docker run --rm --mount "source=$state,target=/state" "$aether_image" node fixture --data-dir /state >/dev/null
live=""
if [ -n "${OPENROUTER_API_KEY:-}" ] && [ -n "${E2E_MODEL:-}" ]; then
  live=1
fi
# `-e NAME` without a value copies the variable from this shell, so the key is
# not on the docker command line.
OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-}" E2E_MODEL="${E2E_MODEL:-}" \
  docker run -d --name "$host" --network "$network" --mount "source=$state,target=/state" \
  -e OPENROUTER_API_KEY -e E2E_MODEL "$openclaw_image" >/dev/null

# Wait until the node has made agent.sock and the gateway is up.
attempt=0
until docker exec "$host" test -S /state/server/agent.sock 2>/dev/null \
  && docker exec "$host" grep -qF '[gateway] ready' /state/logs/gateway.log 2>/dev/null; do
  attempt=$((attempt + 1))
  [ "$attempt" -lt 180 ] || fail "the node or the gateway did not start"
  sleep 1
done
echo "ok: the node and the gateway run"

inspect="$(docker exec "$host" openclaw plugins inspect aether --runtime --json 2>&1)" \
  || fail "openclaw plugins inspect aether failed: $inspect"
for expected in '"status": "loaded"' '"hookCount": 1' '"name": "aether"'; do
  printf '%s\n' "$inspect" | grep -F "$expected" >/dev/null || fail "inspect has no $expected"
done
printf '%s\n' "$inspect" | grep -F '"diagnostics": []' >/dev/null || fail "the plugin has diagnostics: $inspect"
echo "ok: the plugin is loaded with its hook and the aether MCP server"

docker run --rm --mount "source=$state,target=/state" --entrypoint sh "$aether_image" \
  -c 'aether node invite --data-dir /state/server --agent-id server --expires-in 600 > /state/direct-invitation.json'
docker run --rm --network "$network" --mount "source=$state,target=/state" "$aether_image" \
  node direct --data-dir /state/client import /state/direct-invitation.json >/dev/null
# The plugin connects to agent.sock a few seconds after the gateway starts.
# Until then the node fails the task as not available, so try again.
attempt=0
result=""
until printf '%s\n' "$result" | grep -E '"text":"[^"]+' >/dev/null; do
  attempt=$((attempt + 1))
  [ "$attempt" -le "${live:+6}${live:-20}" ] || fail "no OpenClaw answer; last result: $result"
  # A real model has a rate limit (free OpenRouter models allow a few requests
  # a minute), so wait longer between attempts.
  [ "$attempt" -eq 1 ] || sleep "${live:+20}${live:-3}"
  result="$(docker run --rm --network "$network" --mount "source=$state,target=/state" "$aether_image" \
    node request --data-dir /state/client --agent-id server --address "$host:4242" --scope direct \
    --text 'Answer in one short sentence: what is 2 + 2?' --authorization /state/authorization.json 2>&1)" || true
done
printf '%s\n' "$result"

if [ -z "$live" ]; then
  printf '%s\n' "$result" | grep -F '"text":"fake OpenClaw result"' >/dev/null \
    || fail "the answer is not the mock answer"
  echo "ok: the task was answered by OpenClaw"

  # The model request of the aether agent carries the framed task and no
  # aether__ tool.
  task_line="$(docker exec "$host" grep -F '"aetherTask":true' /state/logs/model.log | tail -n 1)"
  [ -n "$task_line" ] || fail "the model did not get the framed Aether task"
  if printf '%s\n' "$task_line" | grep -F 'aether__' >/dev/null; then
    fail "the aether agent got aether__ tools: $task_line"
  fi
  echo "ok: the aether agent got the framed task and no aether__ tool"

  # The owner's main agent gets the caller tools and no owner tool.
  docker exec "$host" openclaw agent --agent main --message 'e2e main probe' >/dev/null 2>&1 \
    || fail "openclaw agent --agent main failed"
  main_line="$(docker exec "$host" grep -F '"mainProbe":true' /state/logs/model.log | tail -n 1)"
  printf '%s\n' "$main_line" | grep -F 'aether__' >/dev/null \
    || fail "the main agent got no aether__ tool: $main_line"
  for owner_tool in settings invites access change groups audit name-set; do
    if printf '%s\n' "$main_line" | grep -E "aether__[a-z.-]*$owner_tool" >/dev/null; then
      fail "the main agent got an owner tool ($owner_tool): $main_line"
    fi
  done
  echo "ok: the main agent got only caller tools: $(printf '%s\n' "$main_line" | grep -oE 'aether__[a-zA-Z._-]+' | sort -u | tr '\n' ' ')"
else
  printf '%s\n' "$result" | grep -E '"text":"[^"]+' >/dev/null || fail "the live answer is empty"
  echo "ok: the task was answered by $E2E_MODEL through OpenClaw"
fi
echo "PASS"
