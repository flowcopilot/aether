"""Tests of the parts of the plugin that do not need Hermes.

Run from the repository root: ``python3 -m unittest discover -s integrations/hermes-plugin/tests``.
"""

from __future__ import annotations

import importlib.util
import json
import os
import signal
import socket
import sys
import tempfile
import textwrap
import threading
import time
import unittest
from pathlib import Path

PLUGIN = Path(__file__).resolve().parent.parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(f"aether_plugin_{name}", PLUGIN / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


node = _load("node")
mcp_client = _load("mcp_client")


class FakeNode:
    """A node agent socket that sends one task and keeps what the agent says."""

    def __init__(self, directory: Path, messages: list[dict]) -> None:
        self.path = node.socket_path(directory)
        self.received: list[dict] = []
        self._server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server.bind(str(self.path))
        self._server.listen(1)
        self._messages = messages
        self._done = threading.Event()
        threading.Thread(target=self._serve, daemon=True).start()

    def _serve(self) -> None:
        connection, _ = self._server.accept()
        reader = connection.makefile("rb")
        self.received.append(json.loads(reader.readline()))
        for message in self._messages:
            connection.sendall(json.dumps(message).encode() + b"\n")
        for line in reader:
            message = json.loads(line)
            self.received.append(message)
            if message["type"] in ("result", "failed", "cancelled"):
                break
        connection.close()
        self._done.set()

    def wait(self) -> None:
        assert self._done.wait(5), "the fake node must finish"
        self._server.close()


class AgentConnectionTest(unittest.TestCase):
    def test_the_agent_says_ready_reads_a_task_and_returns_its_result(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fake = FakeNode(
                Path(directory),
                [
                    {"type": "task", "id": "task-1", "instructions": "Do the task.", "text": "hello"},
                ],
            )
            connection = node.AgentConnection(fake.path, timeout=5)
            message = next(connection.messages())
            self.assertEqual(message["type"], "task")
            connection.progress(message["id"])
            connection.result(message["id"], "answer")
            fake.wait()
            connection.close()
            self.assertEqual(
                fake.received,
                [
                    {"type": "ready", "protocol": 1},
                    {"type": "progress", "id": "task-1"},
                    {"type": "result", "id": "task-1", "text": "answer"},
                ],
            )

    def test_the_framed_task_cannot_start_a_slash_command(self) -> None:
        framed = node.frame_task("Do the task.", "/reset everything")
        self.assertTrue(framed.startswith("[Aether task"))
        self.assertIn("untrusted", framed)
        self.assertTrue(framed.endswith("/reset everything"))


FAKE_MCP_SERVER = textwrap.dedent(
    """
    import json, os, sys, time
    for line in sys.stdin:
        message = json.loads(line)
        if message.get("method") == "initialize":
            reply = {"jsonrpc": "2.0", "id": message["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake", "version": "1"}}}
        elif message.get("method") == "tools/call":
            params = message["params"]
            with open(os.path.join(os.path.dirname(sys.argv[0]), "calls"), "a") as calls:
                calls.write(params["name"] + "\\n")
            if params["name"] == "crash":
                os._exit(1)
            if params["name"] == "sleep":
                time.sleep(30)
            failed = params["name"] != "aether.task.send"
            text = json.dumps({"tool": params["name"], "arguments": params["arguments"], "argv": sys.argv[1:], "pid": os.getpid()})
            reply = {"jsonrpc": "2.0", "id": message["id"], "result": {"content": [{"type": "text", "text": text}], "isError": failed}}
        else:
            continue
        sys.stdout.write(json.dumps(reply) + "\\n")
        sys.stdout.flush()
    """
)


def _gone(pid: int) -> bool:
    """Whether the child process ``pid`` has stopped (it reaps a zombie)."""
    try:
        return os.waitpid(pid, os.WNOHANG)[0] == pid
    except ChildProcessError:
        return True


class McpClientTest(unittest.TestCase):
    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        directory = Path(self._directory.name)
        script = directory / "fake_mcp.py"
        script.write_text(FAKE_MCP_SERVER)
        self.aether = directory / "aether"
        self.aether.write_text(f"#!/bin/sh\nexec {sys.executable} -I {script} \"$@\"\n")
        os.chmod(self.aether, 0o755)

    def tearDown(self) -> None:
        mcp_client.close_all()
        self._directory.cleanup()

    def _send(self, tool: str = "aether.task.send", timeout: float = 10) -> dict:
        _, text = mcp_client.call_tool(str(self.aether), Path("/data"), tool, {}, timeout=timeout)
        return json.loads(text)

    def test_a_tool_call_runs_aether_mcp_serve_with_the_data_directory(self) -> None:
        is_error, text = mcp_client.call_tool(
            str(self.aether), Path("/data"), "aether.task.send", {"agent": "ana#0042", "text": "hi"}, timeout=10
        )
        self.assertFalse(is_error)
        reply = json.loads(text)
        del reply["pid"]
        self.assertEqual(
            reply,
            {
                "tool": "aether.task.send",
                "arguments": {"agent": "ana#0042", "text": "hi"},
                "argv": ["mcp", "serve", "--data-dir", "/data"],
            },
        )

    def test_a_tool_error_is_reported_as_an_error(self) -> None:
        is_error, _ = mcp_client.call_tool(str(self.aether), Path("/data"), "aether.inbox.read", {}, timeout=10)
        self.assertTrue(is_error)

    def test_a_missing_command_is_an_mcp_error(self) -> None:
        with self.assertRaises(mcp_client.McpError):
            mcp_client.call_tool("/nonexistent/aether", Path("/data"), "aether.agents.list", {}, timeout=5)

    def test_two_calls_use_one_process(self) -> None:
        self.assertEqual(self._send()["pid"], self._send()["pid"])

    def test_a_call_after_the_process_stopped_starts_a_new_process(self) -> None:
        first = self._send()["pid"]
        os.kill(first, signal.SIGKILL)
        while not _gone(first):
            time.sleep(0.01)
        self.assertNotEqual(self._send()["pid"], first)

    def test_a_call_that_the_process_received_is_not_sent_again(self) -> None:
        first = self._send()["pid"]
        with self.assertRaises(mcp_client.McpError):
            self._send("crash")
        self.assertNotEqual(self._send()["pid"], first)
        calls = (Path(self._directory.name) / "calls").read_text().split()
        self.assertEqual(calls.count("crash"), 1)

    def test_a_call_that_times_out_stops_the_process(self) -> None:
        first = self._send()["pid"]
        with self.assertRaises(mcp_client.McpError):
            self._send("sleep", timeout=0.5)
        self.assertTrue(_gone(first))
        self.assertNotEqual(self._send()["pid"], first)

if __name__ == "__main__":
    unittest.main()
