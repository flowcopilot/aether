#!/bin/sh
# The end-to-end test of the demo: alice (Hermes) and bob (OpenClaw) start,
# invite each other, and each sends the other a task.
#
#   sh demo/e2e.sh            # build, test, then remove the containers
#   KEEP=1 sh demo/e2e.sh     # keep them running for the demo
#
# By default mock models answer. With OPENROUTER_API_KEY and DEMO_MODEL set,
# a real model answers and the test checks only that each answer has text.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
compose="docker compose -f $here/compose.yml"
call="python3 $here/mcp-call.py"
live=""
if [ -n "${OPENROUTER_API_KEY:-}" ] && [ -n "${DEMO_MODEL:-}" ]; then
  live=1
fi

logs() {
  $compose ps -a
  $compose logs --tail 40 alice bob model-alice model-bob 2>&1 || true
  $compose exec -T alice cat /opt/data/logs/demo/node.log 2>/dev/null || true
  $compose exec -T bob cat /root/.openclaw/logs/demo/node.log 2>/dev/null || true
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  logs >&2
  exit 1
}

cleanup() {
  [ -n "${KEEP:-}" ] || $compose down -v >/dev/null 2>&1 || true
}

trap cleanup EXIT INT TERM
$compose down -v >/dev/null 2>&1 || true
$compose up -d --build >/dev/null 2>&1 || fail "docker compose up failed"

# Each agent connects to its node's agent.sock a few seconds after start.
attempt=0
until $compose logs alice 2>&1 | grep -qF 'aether connected' \
  && $compose logs bob 2>&1 | grep -qF '[gateway] ready'; do
  attempt=$((attempt + 1))
  [ "$attempt" -lt 180 ] || fail "the Hermes or the OpenClaw gateway did not start"
  sleep 1
done
echo "ok: alice runs Hermes and bob runs OpenClaw"

agents="$(sh "$here/connect.sh")" || fail "alice and bob did not connect"
alice_agent="$(printf '%s\n' "$agents" | sed -n 's/^ALICE_AGENT=//p')"
bob_agent="$(printf '%s\n' "$agents" | sed -n 's/^BOB_AGENT=//p')"
echo "ok: bob can send to '$alice_agent' and alice can send to '$bob_agent'"

# send <from> <to agent> <text>: send a task and print the answer text. The
# plugin connects a few seconds after its gateway starts; until then the node
# fails the task as not available, so try again.
send() {
  attempt=0
  while :; do
    attempt=$((attempt + 1))
    result="$($call "$1" aether.task.send "{\"agent\":\"$2\",\"text\":\"$3\"}" 2>&1)" || true
    if printf '%s\n' "$result" | grep -qF '"status":"answered"'; then
      printf '%s\n' "$result"
      return 0
    fi
    [ "$attempt" -lt "${live:+6}${live:-15}" ] || fail "$1 got no answer from $2: $result"
    sleep "${live:+20}${live:-3}"
  done
}

# expect <answer> <text>: in mock mode, the answer holds the mock's text.
expect() {
  if [ -n "$live" ]; then
    printf '%s\n' "$1" | grep -qE '"answer":"[^"]+' || fail "the answer is empty: $1"
  else
    printf '%s\n' "$1" | grep -qF "$2" || fail "the answer is not '$2': $1"
  fi
}

answer="$(send bob "$alice_agent" 'Answer in one short sentence: what is 2 + 2?')"
expect "$answer" 'fake Hermes result'
echo "ok: bob -> alice, answered by Hermes: $answer"

answer="$(send alice "$bob_agent" 'Answer in one short sentence: what is 3 + 3?')"
expect "$answer" 'fake OpenClaw result'
echo "ok: alice -> bob, answered by OpenClaw: $answer"

if [ -z "$live" ]; then
  # Each model got the task in the Aether frame, so the task went through
  # the agent and not around it.
  for name in alice bob; do
    $compose logs "model-$name" 2>&1 | grep -qF '"aetherTask":true' \
      || fail "the model of $name did not get the framed Aether task"
  done
  echo "ok: both models got the framed Aether task"

  # A task from another person gets no send tool: it cannot send tasks with
  # the owner's access.
  for name in alice bob; do
    $compose logs --no-log-prefix "model-$name" 2>&1 | grep -F '"aetherTask":true' \
      | grep -qE '"aether_|"aether__' && fail "an Aether task on $name got a send tool"
  done
  echo "ok: the Aether tasks got no send tool"

  # The owners' own agents get the send tools, so each owner can ask their
  # agent to send a task to the other person.
  $compose exec -T alice hermes chat -q 'e2e owner probe' >/dev/null 2>&1 \
    || fail "hermes chat failed on alice"
  $compose logs --no-log-prefix model-alice 2>&1 | grep -F 'e2e owner probe' \
    | grep -qF '"aether_task_send"' || fail "alice's Hermes has no aether_task_send"
  # OpenClaw adds text after the message, so find it by the mock's
  # mainProbe flag, which matches 'e2e main probe' anywhere in the request.
  $compose exec -T bob openclaw agent --agent main --message 'e2e main probe' >/dev/null 2>&1 \
    || fail "openclaw agent failed on bob"
  $compose logs --no-log-prefix model-bob 2>&1 | grep -F '"mainProbe":true' \
    | grep -qF 'aether__aether-task-send' || fail "bob's OpenClaw has no aether task send tool"
  echo "ok: the owners' agents have the send tools"
fi
echo "PASS"
