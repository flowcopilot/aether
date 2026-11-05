#!/bin/sh
# The host side of the OpenClaw container test: the model, the Aether node
# with the connected adapter, and the OpenClaw gateway with the aether plugin.
# Logs go to /state/logs so the test script can read them.
#
# By default the model is the local mock. With OPENROUTER_API_KEY and
# E2E_MODEL (for example openrouter/auto) set, a real OpenRouter model answers.
set -eu

mkdir -p /state/logs
if [ -n "${OPENROUTER_API_KEY:-}" ] && [ -n "${E2E_MODEL:-}" ]; then
  openclaw config set --batch-json "[
    {\"path\":\"models.mode\",\"value\":\"merge\"},
    {\"path\":\"agents.defaults.model.primary\",\"value\":\"$E2E_MODEL\"}
  ]" >/state/logs/config.log 2>&1
else
  node /opt/e2e/mock-model.mjs 2>/state/logs/model.log &
fi
aether node serve --adapter connected --policy /state/policy.json \
  --data-dir /state/server --port 4242 >/state/logs/node.log 2>&1 &

# The plugin connects to agent.sock and retries every 2 seconds, so the
# gateway may start before the node has made the socket.
exec openclaw gateway run >/state/logs/gateway.log 2>&1
