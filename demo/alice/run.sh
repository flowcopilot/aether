#!/bin/sh
# Alice: the Aether node with the connected adapter, and the Hermes gateway
# with the aether plugin. The node's admin page is forwarded to port 8441.
#
# By default the model is the mock at $MOCK_MODEL_URL. With
# OPENROUTER_API_KEY and DEMO_MODEL set, an OpenRouter model answers.
set -eu

data_dir="$AETHER_DATA_DIR"
logs=/opt/data/logs/demo
mkdir -p "$logs" /opt/data/plugins

if [ -n "${OPENROUTER_API_KEY:-}" ] && [ -n "${DEMO_MODEL:-}" ]; then
  model="model:
  provider: openrouter
  default: $DEMO_MODEL"
else
  model="model:
  provider: custom
  default: mock-model
  base_url: $MOCK_MODEL_URL
  api_key: mock"
fi
cat > /opt/data/config.yaml <<CONFIG
$model
plugins:
  enabled:
    - aether
  entries:
    aether:
      settings:
        data_dir: $data_dir
        aether_bin: aether
gateway:
  platforms:
    aether:
      enabled: true
# The owner's CLI chat may send tasks. A task from another person (the
# aether platform) gets no tool: without a saved list, Hermes gives a
# platform its default tools and every plugin toolset, the send tools too.
platform_toolsets:
  cli:
    - hermes-cli
    - aether
  aether: []
# Hermes turns on a plugin toolset on every platform unless the platform
# lists it here. This keeps the send tools off for the aether platform.
known_plugin_toolsets:
  aether:
    - aether
# Give the model every tool at once, so it sees the aether tools without
# a tool search first.
tools:
  tool_search:
    enabled: off
CONFIG

rm -rf /opt/data/plugins/aether
cp -r /opt/aether-plugin /opt/data/plugins/aether
aether node serve --adapter connected --data-dir "$data_dir" --port 4242 \
  --admin-port 8441 --hub-url "${AETHER_HUB_URL:-off}" --invite-name alice \
  >"$logs/node.log" 2>&1 &

# The admin page listens on 127.0.0.1 only. Forward the container address
# to it, so the host can open https://127.0.0.1:8441/.
socat "TCP-LISTEN:8441,bind=$(hostname -i | cut -d' ' -f1),fork,reuseaddr" TCP:127.0.0.1:8441 &

# The Hermes web dashboard. It listens on 127.0.0.1, where it needs no
# login. The forward below makes it http://127.0.0.1:9119/ on the host.
hermes dashboard --host 127.0.0.1 --port 9119 --no-open --skip-build \
  >"$logs/dashboard.log" 2>&1 &
socat "TCP-LISTEN:9119,bind=$(hostname -i | cut -d' ' -f1),fork,reuseaddr" TCP:127.0.0.1:9119 &

# The plugin connects to agent.sock and tries again every 2 seconds, so the
# gateway may start before the node has made the socket.
exec hermes gateway run --no-supervise --force -v
