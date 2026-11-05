"""Calls the caller tools of ``aether mcp serve`` (stdlib only).

The send tools use the same MCP server as every other agent that sends Aether
tasks, so contacts, hub names, the outbox and offline delivery work the same
way.

One ``aether mcp serve`` process serves all the calls for one command and
data directory. Hermes calls the tools from more than one thread, so a lock
lets one call at a time use the process. When the process stops, the next
call starts a new one.
"""

from __future__ import annotations

import atexit
import json
import queue
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Optional

_PROTOCOL_VERSION = "2025-06-18"


class McpError(Exception):
    """The MCP server could not run the call."""


class _Stale(Exception):
    """A kept process stopped before the request was written."""


class _Server:
    """One ``aether mcp serve`` process, started on the first call."""

    def __init__(self, command: list[str]) -> None:
        self._command = command
        self.lock = threading.Lock()
        self._process: Optional[subprocess.Popen] = None
        self._lines: "queue.Queue[Optional[bytes]]" = queue.Queue()
        self._next_id = 0

    def call(self, tool: str, arguments: dict[str, Any], deadline: float) -> dict:
        """Sends one ``tools/call`` and returns its reply. The caller holds
        ``lock``."""
        if self._process is not None:
            try:
                return self._call(tool, arguments, deadline)
            except _Stale:
                self.close()
        self._start(deadline)
        try:
            return self._call(tool, arguments, deadline)
        except _Stale as error:
            self.close()
            raise McpError("the Aether MCP server stopped") from error

    def _start(self, deadline: float) -> None:
        try:
            process = subprocess.Popen(
                self._command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
        except OSError as error:
            raise McpError(f"cannot start {self._command[0]}: {error}") from error
        lines: "queue.Queue[Optional[bytes]]" = queue.Queue()

        def read() -> None:
            assert process.stdout is not None
            try:
                for line in process.stdout:
                    lines.put(line)
            except (OSError, ValueError):
                pass
            lines.put(None)

        threading.Thread(target=read, name="aether-mcp", daemon=True).start()
        self._process = process
        self._lines = lines
        try:
            request_id = self._request("initialize", {
                "protocolVersion": _PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "hermes-aether", "version": "2.0.0"},
            })
            if "error" in self._response(request_id, deadline):
                raise McpError("the Aether MCP server refused the session")
            self._write({"jsonrpc": "2.0", "method": "notifications/initialized"})
        except OSError as error:
            self.close()
            raise McpError(f"the Aether MCP server failed: {error}") from error
        except BaseException:
            self.close()
            raise

    def _call(self, tool: str, arguments: dict[str, Any], deadline: float) -> dict:
        assert self._process is not None
        if self._process.poll() is not None:
            raise _Stale()
        try:
            request_id = self._request("tools/call", {"name": tool, "arguments": arguments})
        except OSError as error:
            # Nothing reached the server, so the call can go to a new process.
            raise _Stale() from error
        try:
            return self._response(request_id, deadline)
        except BaseException:
            # The request was written: it is not sent again.
            self.close()
            raise

    def _request(self, method: str, params: dict) -> int:
        self._next_id += 1
        self._write({"jsonrpc": "2.0", "id": self._next_id, "method": method, "params": params})
        return self._next_id

    def _write(self, message: dict) -> None:
        assert self._process is not None and self._process.stdin is not None
        self._process.stdin.write(json.dumps(message).encode("utf-8") + b"\n")
        self._process.stdin.flush()

    def _response(self, request_id: int, deadline: float) -> dict:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise McpError("the Aether MCP server did not answer in time")
            try:
                line = self._lines.get(timeout=remaining)
            except queue.Empty as error:
                raise McpError("the Aether MCP server did not answer in time") from error
            if line is None:
                raise McpError("the Aether MCP server stopped")
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if isinstance(message, dict) and message.get("id") == request_id:
                return message

    def close(self, wait: float = 0) -> None:
        """Stops the process. It waits ``wait`` seconds for a clean exit,
        then kills it."""
        process, self._process = self._process, None
        if process is None:
            return
        try:
            if process.stdin is not None:
                process.stdin.close()
        except OSError:
            pass
        try:
            if wait > 0:
                process.wait(timeout=wait)
        except subprocess.TimeoutExpired:
            pass
        if process.poll() is None:
            process.kill()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        if process.stdout is not None:
            try:
                process.stdout.close()
            except OSError:
                pass


_servers: dict[tuple[str, ...], _Server] = {}
_servers_lock = threading.Lock()


def _server(command: list[str]) -> _Server:
    with _servers_lock:
        server = _servers.get(tuple(command))
        if server is None:
            server = _servers[tuple(command)] = _Server(command)
        return server


@atexit.register
def close_all() -> None:
    """Stops all the processes. It does not wait for a call that runs."""
    with _servers_lock:
        servers = list(_servers.values())
    for server in servers:
        if server.lock.acquire(timeout=1):
            try:
                server.close(wait=1)
            finally:
                server.lock.release()


def call_tool(
    aether_bin: str,
    data_dir: Path,
    tool: str,
    arguments: dict[str, Any],
    timeout: float = 60.0,
) -> tuple[bool, str]:
    """Runs ``tool`` and returns ``(is_error, text)``."""
    deadline = time.monotonic() + timeout
    server = _server([aether_bin, "mcp", "serve", "--data-dir", str(data_dir)])
    if not server.lock.acquire(timeout=max(timeout, 0)):
        raise McpError("the Aether MCP server did not answer in time")
    try:
        reply = server.call(tool, arguments, deadline)
    finally:
        server.lock.release()
    if "error" in reply:
        message = (reply.get("error") or {}).get("message") or "unknown error"
        return True, str(message)
    result = reply.get("result") or {}
    text = "\n".join(
        str(part.get("text", ""))
        for part in result.get("content") or []
        if isinstance(part, dict) and part.get("type") == "text"
    )
    return bool(result.get("isError")), text
