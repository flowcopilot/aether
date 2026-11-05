#!/bin/sh
# Bob: the Aether node with the connected adapter, and the OpenClaw gateway
# with the aether plugin. The node's admin page is forwarded to port 8442.
#
# By default the model is the mock at $MOCK_MODEL_URL. With
# OPENROUTER_API_KEY and DEMO_MODEL set, an OpenRouter model answers.
set -eu

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

aether node serve --adapter connected --data-dir "$data_dir" --port 4242 \
  --admin-port 8442 --hub-url "${AETHER_HUB_URL:-off}" --invite-name bob \
  >"$logs/node.log" 2>&1 &

# The admin page listens on 127.0.0.1 only. Forward the container address
# to it, so the host can open https://127.0.0.1:8442/.
socat "TCP-LISTEN:8442,bind=$(hostname -i | cut -d' ' -f1),fork,reuseaddr" TCP:127.0.0.1:8442 &

# The OpenClaw Control UI, served by the gateway on 127.0.0.1:18789.
# `openclaw dashboard --no-open` prints the URL with a sign-in token.
socat "TCP-LISTEN:18789,bind=$(hostname -i | cut -d' ' -f1),fork,reuseaddr" TCP:127.0.0.1:18789 &

# The plugin connects to agent.sock and tries again every 2 seconds, so the
# gateway may start before the node has made the socket.
exec openclaw gateway run
