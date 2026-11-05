"""Tests of the ``aether`` platform adapter against the Hermes gateway code.

They run only where Hermes is importable, for example:
``PYTHONPATH=<hermes-agent checkout> <hermes venv>/bin/python -m unittest discover -s integrations/hermes-plugin/tests``.
"""

from __future__ import annotations

import asyncio
import importlib.util
import json
import socket
import sys
import tempfile
import threading
import unittest
from pathlib import Path

PLUGIN = Path(__file__).resolve().parent.parent

try:
    from gateway.config import PlatformConfig
    from gateway.platform_registry import PlatformEntry, platform_registry
    from gateway.platforms.event import ProcessingOutcome
except ImportError:  # Hermes is not installed here.
    PlatformConfig = None


class Context:
    """The parts of the Hermes plugin context that ``register`` uses. The
    platform goes into the real registry, so Hermes checks its fields."""

    def __init__(self, settings: dict) -> None:
        self._settings = settings
        self.tools: dict[str, dict] = {}
        self.platform = None

    def get_config(self, key, default=None):
        return self._settings.get(key, default)

    def register_tool(self, name, toolset, schema, handler, **kwargs):
        self.tools[name] = {"toolset": toolset, "schema": schema, "handler": handler, **kwargs}

    def register_platform(self, name, label, adapter_factory, check_fn, validate_config=None,
                          required_env=None, install_hint="", **entry_kwargs):
        self.platform = PlatformEntry(
            name=name, label=label, adapter_factory=adapter_factory, check_fn=check_fn,
            validate_config=validate_config, required_env=required_env or [],
            install_hint=install_hint, source="plugin", plugin_name="aether", **entry_kwargs,
        )
        platform_registry.register(self.platform)


def _register_plugin(data_dir: Path) -> Context:
    spec = importlib.util.spec_from_file_location(
        "aether_plugin", PLUGIN / "__init__.py", submodule_search_locations=[str(PLUGIN)]
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules["aether_plugin"] = module
    assert spec.loader is not None
    spec.loader.exec_module(module)
    context = Context({"data_dir": str(data_dir)})
    module.register(context)
    return context


class Node:
    """A node agent socket: it sends tasks and records the agent's replies."""

    def __init__(self, directory: Path) -> None:
        self._server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server.bind(str(directory / "agent.sock"))
        self._server.listen(1)
        self.replies: "asyncio.Queue[dict]" = asyncio.Queue()
        self._loop = asyncio.get_running_loop()
        self._connection: socket.socket | None = None
        self._connected = threading.Event()
        threading.Thread(target=self._serve, daemon=True).start()

    def _serve(self) -> None:
        connection, _ = self._server.accept()
        self._connection = connection
        reader = connection.makefile("rb")
        for line in reader:
            message = json.loads(line)
            if message["type"] == "ready":
                self._connected.set()
                continue
            self._loop.call_soon_threadsafe(self.replies.put_nowait, message)

    async def send(self, message: dict) -> None:
        await asyncio.to_thread(self._connected.wait, 5)
        assert self._connection is not None
        self._connection.sendall(json.dumps(message).encode() + b"\n")

    async def reply(self) -> dict:
        return await asyncio.wait_for(self.replies.get(), 5)

    def close(self) -> None:
        if self._connection is not None:
            self._connection.close()
        self._server.close()


@unittest.skipIf(PlatformConfig is None, "Hermes is not importable")
class AetherAdapterTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.data_dir = Path(self._directory.name)
        self.node = Node(self.data_dir)
        self.context = _register_plugin(self.data_dir)
        self.adapter = self.context.platform.adapter_factory(PlatformConfig(enabled=True))
        self.events = []

        async def handle(event):
            self.events.append(event)
            if "fail" in event.text:
                await self.adapter.on_processing_complete(event, ProcessingOutcome.FAILURE)
                return
            await self.adapter.send(event.source.chat_id, "working on it")
            await self.adapter.send(event.source.chat_id, "the answer", metadata={"notify": True})

        self.adapter.handle_message = handle  # the gateway's session loop
        self.adapter._message_handler = object()
        await self.adapter.connect()

    async def asyncTearDown(self) -> None:
        await self.adapter.disconnect()
        self.node.close()
        self._directory.cleanup()

    async def test_a_task_becomes_one_session_message_and_the_reply_its_result(self) -> None:
        await self.node.send({"type": "task", "id": "task-1", "instructions": "Do the task.", "text": "hello"})
        self.assertEqual(await self.node.reply(), {"type": "progress", "id": "task-1"})
        self.assertEqual(await self.node.reply(), {"type": "result", "id": "task-1", "text": "the answer"})
        (event,) = self.events
        self.assertEqual(event.source.chat_id, "task-1")
        self.assertEqual(event.message_id, "task-1")
        self.assertTrue(event.text.startswith("[Aether task"))
        self.assertIn("Do the task.", event.text)
        self.assertTrue(event.text.endswith("hello"))

    async def test_a_failed_turn_fails_the_task_without_text(self) -> None:
        await self.node.send({"type": "task", "id": "task-2", "instructions": "Do the task.", "text": "fail"})
        self.assertEqual(await self.node.reply(), {"type": "failed", "id": "task-2"})

    def test_the_platform_skips_the_gateway_allowlist(self) -> None:
        self.assertTrue(self.context.platform.trusted_inbound)
        self.assertTrue(self.adapter.authorization_is_upstream)

    def test_the_send_tools_are_the_aether_toolset(self) -> None:
        self.assertEqual(
            sorted(self.context.tools),
            [
                "aether_agents_find",
                "aether_agents_list",
                "aether_inbox_list",
                "aether_inbox_read",
                "aether_people_find",
                "aether_task_cancel",
                "aether_task_send",
            ],
        )
        self.assertEqual({tool["toolset"] for tool in self.context.tools.values()}, {"aether"})


if __name__ == "__main__":
    unittest.main()
