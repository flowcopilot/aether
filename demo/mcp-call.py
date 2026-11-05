#!/usr/bin/env python3
"""Call one tool of `aether mcp serve` in a demo container.

    python3 demo/mcp-call.py <service> <tool> '<json arguments>'

It prints the tool's text result and exits 1 when the tool reports an error.
The demo scripts use it to drive the same tools as the agents and the admin
page: invites, contacts and tasks.
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    service, tool, arguments = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
    process = subprocess.Popen(
        ["docker", "compose", "-f", os.path.join(HERE, "compose.yml"), "exec", "-T", service,
         "aether", "mcp", "serve"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )

    def send(message: dict) -> None:
        process.stdin.write(json.dumps(message) + "\n")
        process.stdin.flush()

    def answer(request_id: int) -> dict:
        for line in process.stdout:
            message = json.loads(line)
            if message.get("id") == request_id:
                return message
        raise SystemExit(f"mcp-call: the server stopped before answer {request_id}")

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "aether-demo", "version": "1"}}})
        answer(1)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
              "params": {"name": tool, "arguments": arguments}})
        message = answer(2)
    finally:
        process.stdin.close()
        process.wait(timeout=10)

    if "error" in message:
        print(json.dumps(message["error"]), file=sys.stderr)
        return 1
    result = message["result"]
    print("".join(part.get("text", "") for part in result.get("content", [])))
    return 1 if result.get("isError") else 0


if __name__ == "__main__":
    sys.exit(main())
