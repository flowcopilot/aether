#!/bin/sh
# Bob: the Aether node with the connected adapter, and the OpenClaw gateway
# with the aether plugin. The node's admin page is forwarded to port 8442.
#
# By default the model is the mock at $MOCK_MODEL_URL. With
# OPENROUTER_API_KEY and DEMO_MODEL set, an OpenRouter model answers.
set -eu
. /opt/demo/node.sh

data_dir=/root/.aether/host
logs=/root/.openclaw/logs/demo
mkdir -p "$logs"

if [ -n "${OPENROUTER_API_KEY:-}" ] && [ -n "${DEMO_MODEL:-}" ]; then
  openclaw config set --batch-json "[
    {\"path\":\"models.mode\",\"value\":\"merge\"},
    {\"path\":\"agents.defaults.model.primary\",\"value\":\"openrouter/$DEMO_MODEL\"}
  ]" >"$logs/config.log" 2>&1
else
  openclaw config set models.providers.mock.baseUrl "$MOCK_MODEL_URL" >"$logs/config.log" 2>&1
fi

# The node page is https://127.0.0.1:8442/ on the host.
start_node "$data_dir" 8442 bob "$logs/node.log"

# The OpenClaw Control UI, served by the gateway on 127.0.0.1:18789.
# `openclaw dashboard --no-open` prints the URL with a sign-in token.
forward 18789

# The plugin connects to agent.sock and tries again every 2 seconds, so the
# gateway may start before the node has made the socket.
exec openclaw gateway run
