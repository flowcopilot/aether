#!/usr/bin/env python3
"""Call one tool of `aether mcp serve` in a demo container.

    python3 demo/mcp-call.py <service> <tool> '<json arguments>'

It prints the tool's text result and exits 1 when the tool reports an error.
The demo scripts use it to drive the same tools as the agents and the admin
page: invites, contacts and tasks. It uses the MCP client of the Hermes
plugin.
"""

import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CLIENT = os.path.join(HERE, "..", "integrations", "hermes-plugin", "mcp_client.py")


def _client():
    spec = importlib.util.spec_from_file_location("aether_mcp_client", CLIENT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    service, tool, arguments = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
    client = _client()
    command = ["docker", "compose", "-f", os.path.join(HERE, "compose.yml"), "exec", "-T", service,
               "aether", "mcp", "serve"]
    try:
        is_error, text = client.call_tool(command, tool, arguments, timeout=120)
    except client.McpError as error:
        print(f"mcp-call: {error}", file=sys.stderr)
        return 1
    print(text)
    return 1 if is_error else 0


if __name__ == "__main__":
    sys.exit(main())
