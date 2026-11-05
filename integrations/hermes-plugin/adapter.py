"""The ``aether`` gateway platform: each task that the Aether node admits
arrives as a message in its own gateway session, and the reply goes back to
the node as the task result.

The node has already checked the caller's grant, so the platform waives the
gateway allowlist (``trusted_inbound``). The adapter never sends the Hermes
setup (model, tools, MCP servers) to the node: only the reply text.
"""

from __future__ import annotations

import asyncio
import logging
import threading
from pathlib import Path
from typing import Any, Optional

from gateway.config import Platform
from gateway.platforms.base import BasePlatformAdapter, SendResult
from gateway.platforms.event import MessageEvent, MessageType, ProcessingOutcome

from .node import AgentConnection, frame_task, socket_path

logger = logging.getLogger(__name__)

# Seconds between attempts to reach the node socket.
_RECONNECT_DELAY = 2.0
# The sender name of every task: the node does not tell the agent who sent it.
_CALLER = "aether-caller"


class AetherAdapter(BasePlatformAdapter):
    """Inbound Aether tasks over the node's agent socket."""

    def __init__(self, config, data_dir: Path, **_kwargs) -> None:
        super().__init__(config=config, platform=Platform("aether"))
        self._socket_path = socket_path(data_dir)
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._connection: Optional[AgentConnection] = None
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        # Task IDs that still wait for an answer. A task is its own chat, so
        # ``chat_id`` is the task ID.
        self._pending: dict[str, MessageEvent] = {}
        self._lock = threading.Lock()

    @property
    def name(self) -> str:
        return "Aether"

    @property
    def authorization_is_upstream(self) -> bool:
        """The node checks each caller's grant before a task gets here."""
        return True

    async def connect(self, **_kwargs) -> bool:
        self._loop = asyncio.get_running_loop()
        self._stop.clear()
        self._thread = threading.Thread(target=self._run, name="aether-node", daemon=True)
        self._thread.start()
        self._mark_connected()
        logger.info("Aether: answering tasks from %s", self._socket_path)
        return True

    async def disconnect(self) -> None:
        self._mark_disconnected()
        self._stop.set()
        connection = self._connection
        if connection is not None:
            connection.close()
        with self._lock:
            self._pending.clear()

    def _run(self) -> None:
        """Attach to the node, and attach again when the node restarts."""
        while not self._stop.is_set():
            try:
                connection = AgentConnection(self._socket_path, timeout=5.0)
            except OSError:
                self._stop.wait(_RECONNECT_DELAY)
                continue
            self._connection = connection
            try:
                for message in connection.messages():
                    self._receive(message)
            except OSError:
                pass
            finally:
                self._connection = None
                connection.close()
                # The node fails the tasks of a closed connection itself.
                with self._lock:
                    self._pending.clear()
            self._stop.wait(_RECONNECT_DELAY)

    def _receive(self, message: dict) -> None:
        task_id = message["id"]
        kind = message.get("type")
        if kind == "task":
            self._start_task(task_id, str(message.get("instructions") or ""), str(message.get("text") or ""))
        elif kind == "cancel":
            self._cancel_task(task_id)

    def _start_task(self, task_id: str, instructions: str, text: str) -> None:
        loop = self._loop
        if loop is None or self._message_handler is None:
            self._reply(lambda connection: connection.failed(task_id))
            return
        event = MessageEvent(
            text=frame_task(instructions, text),
            message_type=MessageType.TEXT,
            message_id=task_id,
            source=self.build_source(
                chat_id=task_id,
                chat_name="aether",
                chat_type="dm",
                user_id=_CALLER,
                user_name=_CALLER,
            ),
        )
        with self._lock:
            self._pending[task_id] = event
        asyncio.run_coroutine_threadsafe(self.handle_message(event), loop)

    def _cancel_task(self, task_id: str) -> None:
        with self._lock:
            event = self._pending.pop(task_id, None)
        loop = self._loop
        if event is None or loop is None:
            return
        session_key = self._event_session_key(event)
        asyncio.run_coroutine_threadsafe(self.cancel_session_processing(session_key), loop)
        self._reply(lambda connection: connection.cancelled(task_id))

    def _finish(self, task_id: str) -> bool:
        """Removes the task; ``False`` when it was already answered."""
        with self._lock:
            return self._pending.pop(task_id, None) is not None

    def _reply(self, send) -> None:
        connection = self._connection
        if connection is None:
            return
        try:
            send(connection)
        except OSError:
            logger.debug("Aether: the node connection closed before a reply")

    async def send(
        self,
        chat_id: str,
        content: str,
        reply_to: Optional[str] = None,
        metadata: Optional[dict[str, Any]] = None,
    ) -> SendResult:
        """The final reply (``metadata['notify']``) is the task result; any
        other send is progress."""
        task_id = str(chat_id)
        if (metadata or {}).get("notify"):
            if self._finish(task_id):
                self._reply(lambda connection: connection.result(task_id, content or ""))
        else:
            with self._lock:
                active = task_id in self._pending
            if active:
                self._reply(lambda connection: connection.progress(task_id))
        return SendResult(success=True, message_id=task_id)

    async def send_typing(self, chat_id: str, metadata=None) -> None:
        return None

    async def get_chat_info(self, chat_id: str) -> dict[str, Any]:
        return {"name": "aether", "type": "dm"}

    async def on_processing_complete(self, event: MessageEvent, outcome: ProcessingOutcome) -> None:
        """Answer the node when a turn ends without a final send: a failure, a
        cancel, or a reply that the gateway streamed."""
        task_id = str(getattr(event, "message_id", "") or "")
        if not task_id or not self._finish(task_id):
            return
        if outcome == ProcessingOutcome.FAILURE:
            self._reply(lambda connection: connection.failed(task_id))
        elif outcome == ProcessingOutcome.CANCELLED:
            self._reply(lambda connection: connection.cancelled(task_id))
        else:
            streamed = getattr(event, "_streamed_final_response", "")
            text = streamed if isinstance(streamed, str) else ""
            self._reply(lambda connection: connection.result(task_id, text))
