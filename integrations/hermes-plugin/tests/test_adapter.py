"""Tests of the ``aether`` platform adapter against the Hermes gateway code.

They run only where Hermes is importable, for example:
``PYTHONPATH=<hermes-agent checkout> <hermes venv>/bin/python -m unittest discover -s integrations/hermes-plugin/tests``.
"""

from __future__ import annotations

import asyncio
import importlib.util
import json
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from test_node import FakeNode

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


@unittest.skipIf(PlatformConfig is None, "Hermes is not importable")
class AetherAdapterTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.data_dir = Path(self._directory.name)
        self.node = FakeNode(self.data_dir / "agent.sock")
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
        await asyncio.to_thread(self.node.accept)
        self.assertEqual(await self._read(), {"type": "ready", "protocol": 1})

    async def asyncTearDown(self) -> None:
        await self.adapter.disconnect()
        self.node.stop()
        self._directory.cleanup()

    async def _send(self, message: dict) -> None:
        self.node.write(json.dumps(message).encode() + b"\n")

    async def _read(self) -> dict:
        return await asyncio.to_thread(self.node.read)

    async def test_a_task_becomes_one_session_message_and_the_reply_its_result(self) -> None:
        await self._send({"type": "task", "id": "task-1", "instructions": "Do the task.", "text": "hello"})
        self.assertEqual(await self._read(), {"type": "progress", "id": "task-1"})
        self.assertEqual(await self._read(), {"type": "result", "id": "task-1", "text": "the answer"})
        (event,) = self.events
        self.assertEqual(event.source.chat_id, "task-1")
        self.assertEqual(event.message_id, "task-1")
        self.assertTrue(event.text.startswith("[Aether task"))
        self.assertIn("Do the task.", event.text)
        self.assertTrue(event.text.endswith("hello"))

    async def test_a_failed_turn_fails_the_task_without_text(self) -> None:
        await self._send({"type": "task", "id": "task-2", "instructions": "Do the task.", "text": "fail"})
        self.assertEqual(await self._read(), {"type": "failed", "id": "task-2"})

    async def test_a_quick_disconnect_and_connect_leaves_one_node_connection(self) -> None:
        await self.adapter.disconnect()
        await self.adapter.connect()
        await asyncio.to_thread(self.node.accept)
        self.assertEqual(await self._read(), {"type": "ready", "protocol": 1})
        # The first link thread must stop, not reconnect after its retry delay.
        deadline = time.monotonic() + 3
        while sum(thread.name == "aether-node" for thread in threading.enumerate()) > 1:
            self.assertLess(time.monotonic(), deadline, "the first link thread is still running")
            await asyncio.sleep(0.05)


if __name__ == "__main__":
    unittest.main()
