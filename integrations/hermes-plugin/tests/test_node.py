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
REPOSITORY = PLUGIN.parent.parent
PROTOCOL = REPOSITORY / "protocol" / "agent-socket"


def _load_plugin():
    spec = importlib.util.spec_from_file_location(
        "aether_plugin_core", PLUGIN / "__init__.py", submodule_search_locations=[str(PLUGIN)]
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules["aether_plugin_core"] = module
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


_load_plugin()
from aether_plugin_core import mcp_client, node, tools  # noqa: E402

CONTRACT = json.loads((PROTOCOL / "contract.json").read_text())
TRANSCRIPT = json.loads((PROTOCOL / "transcript.json").read_text())


def _wait(condition, what: str) -> None:
    deadline = time.monotonic() + 5
    while not condition():
        assert time.monotonic() < deadline, what
        time.sleep(0.001)


class ContractTest(unittest.TestCase):
    def test_the_constants_are_the_contract(self) -> None:
        self.assertEqual(node.PROTOCOL, CONTRACT["protocol"])
        self.assertEqual(node.SOCKET_FILE, CONTRACT["socket_file"])
        self.assertEqual(node.MAX_LINE, CONTRACT["max_line_bytes"])
        self.assertEqual(node.RECONNECT_SECONDS, CONTRACT["reconnect_seconds"])
        self.assertEqual(node.socket_path(Path("/srv")), Path("/srv") / CONTRACT["socket_file"])

    def test_the_default_data_dir_follows_the_contract(self) -> None:
        for example in CONTRACT["data_dir"]["examples"]:
            env = {} if example["env"] is None else {CONTRACT["data_dir"]["env"]: example["env"]}
            self.assertEqual(node.default_data_dir(env, Path(example["home"])), Path(example["data_dir"]), example)

    def test_a_task_is_framed_as_the_contract_shows(self) -> None:
        for example in CONTRACT["frame"]["examples"]:
            self.assertEqual(node.frame_task(example["instructions"], example["text"]), example["framed"])


def _noise(size: int) -> bytes:
    prefix = b'{"type":"noise","pad":"'
    return prefix + b"x" * (size - len(prefix) - 3) + b'"}\n'


class FakeNode:
    """The node end of the agent socket."""

    def __init__(self, path: Path) -> None:
        self._server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server.bind(str(path))
        self._server.listen(4)
        self._server.settimeout(5)
        self._connection: socket.socket | None = None

    def accept(self) -> None:
        self._connection, _ = self._server.accept()
        self._connection.settimeout(5)
        self._reader = self._connection.makefile("rb")

    def write(self, line: bytes) -> None:
        assert self._connection is not None
        try:
            self._connection.sendall(line)
        except OSError:
            pass  # The agent closes on a line that is too long.

    def read(self) -> dict:
        line = self._reader.readline()
        assert line, "the agent must send a line"
        return json.loads(line)

    def closed(self) -> bool:
        """Whether the agent closed the connection without another line."""
        try:
            return self._reader.read() == b""
        except OSError:
            return True

    def hang_up(self) -> None:
        assert self._connection is not None
        self._connection.shutdown(socket.SHUT_WR)

    def close(self) -> None:
        if self._connection is not None:
            self._reader.close()
            self._connection.close()
            self._connection = None

    def stop(self) -> None:
        self.close()
        self._server.close()


class Host:
    """The agent host: it records the turns that the plugin starts and stops."""

    def __init__(self) -> None:
        self.turns: dict[str, list[str]] = {}
        self.stopped: list[str] = []

    def start(self, task_id: str, message: str) -> str:
        self.turns.setdefault(task_id, []).append(message)
        return task_id

    def stop(self, task_id: str) -> None:
        self.stopped.append(task_id)


class TranscriptTest(unittest.TestCase):
    """The plugin replays ``protocol/agent-socket/transcript.json`` as the
    agent."""

    def test_the_agent_replays_the_protocol_transcript(self) -> None:
        for case in TRANSCRIPT["cases"]:
            with self.subTest(case["name"]):
                self._replay(case)

    def _replay(self, case: dict) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = node.socket_path(Path(directory.name))
        fake = FakeNode(path)
        self.addCleanup(fake.stop)
        host = Host()
        received: list[dict] = []

        def on_message(message: dict) -> None:
            received.append(message)
            tasks.receive(message)

        link = node.NodeLink(path, on_message, lambda up: None if up else tasks.close(), retry=0.01)
        tasks = node.Tasks(send=link.send, start=host.start, stop=host.stop)
        stop = threading.Event()
        runner = threading.Thread(target=link.run, args=(stop,), daemon=True)
        runner.start()

        sent = 0
        for step in case["steps"]:
            ignored = step.get("ignored", False)
            if "agent" in step:
                message = step["agent"]
                if message["type"] == "ready":
                    fake.accept()
                    self.assertEqual(fake.read(), message)
                    continue
                # The host answers only after the plugin has read every node line.
                _wait(lambda: len(received) == sent, "the plugin must read the node's lines")
                if message["type"] == "progress":
                    tasks.progress(message["id"])
                elif message["type"] == "result":
                    tasks.result(message["id"], message["text"])
                elif message["type"] == "failed":
                    tasks.failed(message["id"])
                if not ignored:
                    self.assertEqual(fake.read(), message)
            elif "node" in step:
                fake.write(json.dumps(step["node"]).encode() + b"\n")
                sent += 1
            elif "node_line" in step:
                line = step["node_line"].encode() + b"\n"
                fake.write(line)
                # A valid message for no open task still reaches on_message.
                sent += node.parse_node_line(line) is not None
            elif "node_line_bytes" in step:
                fake.write(_noise(step["node_line_bytes"]))
            elif step.get("closes") == "node":
                fake.close()
            elif step.get("closes") == "agent":
                self.assertTrue(fake.closed())
                fake.close()

        _wait(lambda: len(received) == sent, "the plugin must read the node's lines")
        fake.hang_up()
        self.assertTrue(fake.closed(), "the plugin sent a line that the transcript does not have")
        stop.set()
        link.close()
        runner.join(5)

        for task_id, outcome in case["outcomes"].items():
            self.assertEqual(len(host.turns.get(task_id, [])), 1, task_id)
            self.assertEqual(task_id in host.stopped, outcome in ("cancelled", "lost"), task_id)


class NodeLinkTest(unittest.TestCase):
    def test_a_failing_message_handler_closes_the_session_and_the_link_reconnects(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = node.socket_path(Path(directory.name))
        fake = FakeNode(path)
        self.addCleanup(fake.stop)
        host = Host()
        statuses: list[bool] = []

        def stop_turn(task_id: str) -> None:
            if task_id == "task-1":
                raise RuntimeError("Event loop is closed")
            host.stop(task_id)

        def on_status(connected: bool) -> None:
            statuses.append(connected)
            if not connected:
                tasks.close()

        link = node.NodeLink(path, lambda message: tasks.receive(message), on_status, retry=0.01)
        tasks = node.Tasks(send=link.send, start=host.start, stop=stop_turn)
        stop = threading.Event()
        runner = threading.Thread(target=link.run, args=(stop,), daemon=True)
        runner.start()
        self.addCleanup(runner.join, 5)
        self.addCleanup(link.close)
        self.addCleanup(stop.set)

        fake.accept()
        self.assertEqual(fake.read(), {"type": "ready", "protocol": 1})
        for task_id in ("task-1", "task-2"):
            fake.write(json.dumps({"type": "task", "id": task_id, "instructions": "", "text": "hi"}).encode() + b"\n")
        fake.write(b'{"type":"cancel","id":"task-1"}\n')
        self.assertTrue(fake.closed())
        fake.close()
        _wait(lambda: statuses == [True, False], "the session must report that it closed")
        self.assertEqual(host.stopped, ["task-2"])
        fake.accept()
        self.assertEqual(fake.read(), {"type": "ready", "protocol": 1})
        self.assertTrue(runner.is_alive())


class TasksTest(unittest.TestCase):
    def test_a_turn_that_cannot_start_fails_its_task(self) -> None:
        sent: list[dict] = []

        def start(task_id: str, message: str) -> None:
            raise RuntimeError("no gateway")

        tasks = node.Tasks(send=sent.append, start=start, stop=lambda handle: None)
        tasks.receive({"type": "task", "id": "task-1", "instructions": "", "text": "hi"})
        self.assertEqual(sent, [{"type": "failed", "id": "task-1"}])


FAKE_MCP_SERVER = textwrap.dedent(
    """
    import json, os, sys, time
    TOOLS = [
        {"name": "aether.task.send", "description": "Send one task to an agent from aether.agents.list.",
         "inputSchema": {"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object",
                         "properties": {"agent": {"type": "string", "description": "From `aether.agents.list`."},
                                        "limit": {"type": ["integer", "null"], "format": "uint8"}},
                         "required": ["agent"]}},
        {"name": "aether.agents.list", "description": "List the agents.", "inputSchema": {"type": "object", "properties": {}}},
    ]
    for line in sys.stdin:
        message = json.loads(line)
        if message.get("method") == "initialize":
            reply = {"jsonrpc": "2.0", "id": message["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake", "version": "1"}}}
        elif message.get("method") == "tools/list":
            reply = {"jsonrpc": "2.0", "id": message["id"], "result": {"tools": TOOLS}}
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


class Context:
    """The part of the Hermes plugin context that ``register_tools`` uses."""

    def __init__(self) -> None:
        self.tools: dict[str, dict] = {}

    def register_tool(self, name, toolset, schema, handler, **kwargs):
        self.tools[name] = {"toolset": toolset, "schema": schema, "handler": handler, **kwargs}


class McpClientTest(unittest.TestCase):
    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        directory = Path(self._directory.name)
        script = directory / "fake_mcp.py"
        script.write_text(FAKE_MCP_SERVER)
        self.aether = directory / "aether"
        self.aether.write_text(f"#!/bin/sh\nexec {sys.executable} -I {script} \"$@\"\n")
        os.chmod(self.aether, 0o755)
        self.command = mcp_client.caller_command(str(self.aether), Path("/data"))

    def tearDown(self) -> None:
        mcp_client.close_all()
        self._directory.cleanup()

    def _send(self, tool: str = "aether.task.send", timeout: float = 10) -> dict:
        _, text = mcp_client.call_tool(self.command, tool, {}, timeout=timeout)
        return json.loads(text)

    def test_a_tool_call_runs_the_caller_only_server_with_the_data_directory(self) -> None:
        is_error, text = mcp_client.call_tool(
            self.command, "aether.task.send", {"agent": "ana#0042", "text": "hi"}, timeout=10
        )
        self.assertFalse(is_error)
        reply = json.loads(text)
        del reply["pid"]
        self.assertEqual(
            reply,
            {
                "tool": "aether.task.send",
                "arguments": {"agent": "ana#0042", "text": "hi"},
                "argv": ["mcp", "serve", "--caller-only", "--data-dir", "/data"],
            },
        )

    def test_a_tool_error_is_reported_as_an_error(self) -> None:
        is_error, _ = mcp_client.call_tool(self.command, "aether.inbox.read", {}, timeout=10)
        self.assertTrue(is_error)

    def test_a_missing_command_is_an_mcp_error(self) -> None:
        with self.assertRaises(mcp_client.McpError):
            mcp_client.call_tool(["/nonexistent/aether"], "aether.agents.list", {}, timeout=5)

    def test_two_calls_use_one_process(self) -> None:
        self.assertEqual(self._send()["pid"], self._send()["pid"])

    def test_a_call_after_the_process_stopped_starts_a_new_process(self) -> None:
        first = self._send()["pid"]
        os.kill(first, signal.SIGKILL)
        _wait(lambda: _gone(first), "the process must stop")
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

    def test_the_send_tools_are_the_tools_that_the_server_lists(self) -> None:
        context = Context()
        tools.register_tools(context, tools.Settings(Path("/data"), str(self.aether)))
        self.assertEqual(sorted(context.tools), ["aether_agents_list", "aether_task_send"])
        send = context.tools["aether_task_send"]
        self.assertEqual(send["toolset"], "aether")
        self.assertEqual(send["description"], "Send one task to an agent from aether_agents_list.")
        self.assertEqual(
            send["schema"]["parameters"],
            {
                "type": "object",
                "properties": {
                    "agent": {"type": "string", "description": "From `aether_agents_list`."},
                    "limit": {"type": "integer"},
                },
                "required": ["agent"],
            },
        )
        reply = json.loads(send["handler"]({"agent": "ana#0042", "unknown": ""}))
        self.assertEqual(reply["tool"], "aether.task.send")
        self.assertEqual(reply["arguments"], {"agent": "ana#0042", "unknown": ""})

    def test_no_send_tools_without_the_aether_command(self) -> None:
        context = Context()
        tools.register_tools(context, tools.Settings(Path("/data"), "/nonexistent/aether"))
        self.assertEqual(context.tools, {})


def _aether_bin() -> str | None:
    built = REPOSITORY / "target" / "debug" / "aether"
    value = os.environ.get("AETHER_BIN") or (str(built) if built.exists() else None)
    return value


@unittest.skipIf(_aether_bin() is None, "no aether binary: build it or set AETHER_BIN")
class ManifestTest(unittest.TestCase):
    def tearDown(self) -> None:
        mcp_client.close_all()

    def test_the_manifest_names_the_tools_that_the_caller_server_lists(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            listed = mcp_client.list_tools(mcp_client.caller_command(_aether_bin(), Path(directory)))
        manifest = (PLUGIN / "plugin.yaml").read_text().splitlines()
        start = manifest.index("provides_tools:") + 1
        provided = []
        for line in manifest[start:]:
            if not line.startswith("  - "):
                break
            provided.append(line.removeprefix("  - ").strip())
        self.assertEqual(sorted(provided), sorted(tools.hermes_name(tool["name"]) for tool in listed))


if __name__ == "__main__":
    unittest.main()
