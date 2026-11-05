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

from .node import NodeLink, Tasks, socket_path

logger = logging.getLogger(__name__)

# The sender name of every task: the node does not tell the agent who sent it.
_CALLER = "aether-caller"


class AetherAdapter(BasePlatformAdapter):
    """Inbound Aether tasks over the node's agent socket. A task is its own
    chat, so ``chat_id`` is the task ID."""

    def __init__(self, config, data_dir: Path, **_kwargs) -> None:
        super().__init__(config=config, platform=Platform("aether"))
        self._socket_path = socket_path(data_dir)
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._stop = threading.Event()
        self._tasks = Tasks(send=self._send_to_node, start=self._start_turn, stop=self._stop_turn)
        self._link = NodeLink(self._socket_path, on_message=self._tasks.receive, on_status=self._on_status)

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
        threading.Thread(target=self._link.run, args=(self._stop,), name="aether-node", daemon=True).start()
        self._mark_connected()
        logger.info("Aether: answering tasks from %s", self._socket_path)
        return True

    async def disconnect(self) -> None:
        self._mark_disconnected()
        self._stop.set()
        self._link.close()

    def _on_status(self, connected: bool) -> None:
        if not connected:
            self._tasks.close()

    def _send_to_node(self, message: dict) -> None:
        if not self._link.send(message):
            logger.debug("Aether: the node connection closed before a reply")

    def _start_turn(self, task_id: str, text: str) -> MessageEvent:
        loop = self._loop
        if loop is None or self._message_handler is None:
            raise RuntimeError("the gateway is not running")
        event = MessageEvent(
            text=text,
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
        asyncio.run_coroutine_threadsafe(self.handle_message(event), loop)
        return event

    def _stop_turn(self, event: MessageEvent) -> None:
        loop = self._loop
        if loop is not None:
            session_key = self._event_session_key(event)
            asyncio.run_coroutine_threadsafe(self.cancel_session_processing(session_key), loop)

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
            self._tasks.result(task_id, content or "")
        else:
            self._tasks.progress(task_id)
        return SendResult(success=True, message_id=task_id)

    async def send_typing(self, chat_id: str, metadata=None) -> None:
        return None

    async def get_chat_info(self, chat_id: str) -> dict[str, Any]:
        return {"name": "aether", "type": "dm"}

    async def on_processing_complete(self, event: MessageEvent, outcome: ProcessingOutcome) -> None:
        """Answer the node when a turn ends without a final send: a failure, a
        cancel, or a reply that the gateway streamed."""
        task_id = str(getattr(event, "message_id", "") or "")
        if outcome == ProcessingOutcome.FAILURE:
            self._tasks.failed(task_id)
        elif outcome == ProcessingOutcome.CANCELLED:
            self._tasks.cancelled(task_id)
        else:
            streamed = getattr(event, "_streamed_final_response", "")
            self._tasks.result(task_id, streamed if isinstance(streamed, str) else "")
