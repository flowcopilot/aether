"""The ``aether`` toolset: one Hermes tool for each tool that
``aether mcp serve --caller-only`` lists."""

from __future__ import annotations

import json
import logging
import os
import re
import shutil
from functools import partial
from pathlib import Path
from typing import Any, Optional

from .mcp_client import McpError, call_tool, caller_command, list_tools

logger = logging.getLogger(__name__)

_MCP_NAME = re.compile(r"\baether\.([a-z_]+)\.([a-z_]+)\b")


def hermes_name(mcp_name: str) -> str:
    """``aether.task.send`` is ``aether_task_send``: Hermes tool names have no
    dots."""
    return mcp_name.replace(".", "_")


def _hermes_text(text: str) -> str:
    return _MCP_NAME.sub(lambda match: hermes_name(match.group(0)), text)


def _parameters(schema: Any) -> Any:
    """The MCP input schema as Hermes tool parameters: the tool names in the
    descriptions are the Hermes names, and an optional field has its plain
    type instead of ``[type, "null"]``."""
    if isinstance(schema, str):
        return _hermes_text(schema)
    if isinstance(schema, list):
        return [_parameters(item) for item in schema]
    if not isinstance(schema, dict):
        return schema
    result = {}
    for key, value in schema.items():
        if key in ("$schema", "format"):
            continue
        if key == "type" and isinstance(value, list):
            types = [kind for kind in value if kind != "null"]
            value = types[0] if len(types) == 1 else types
        result[key] = _parameters(value)
    return result


def _handler(settings, mcp_tool: str, args: dict, **_: Any) -> str:
    try:
        is_error, text = call_tool(settings.command(), mcp_tool, args or {})
    except McpError as error:
        return json.dumps({"error": str(error)})
    return json.dumps({"error": text}) if is_error else text


def register_tools(ctx, settings) -> None:
    try:
        tools = list_tools(settings.command())
    except McpError as error:
        logger.warning("Aether: no send tools, because %s", error)
        return
    for tool in tools:
        name = hermes_name(tool["name"])
        description = _hermes_text(str(tool.get("description") or ""))
        parameters = _parameters(tool.get("inputSchema") or {"type": "object", "properties": {}})
        ctx.register_tool(
            name=name,
            toolset="aether",
            schema={"name": name, "description": description, "parameters": parameters},
            handler=partial(_handler, settings, tool["name"]),
            description=description,
            check_fn=settings.tools_available,
        )


class Settings:
    """The plugin settings: the node data directory and the ``aether`` command."""

    def __init__(self, data_dir: Path, aether_bin: str) -> None:
        self.data_dir = Path(data_dir).expanduser()
        self.aether_bin = aether_bin
        self._found: Optional[str] = None

    def command(self) -> list[str]:
        return caller_command(self.aether_bin, self.data_dir)

    def tools_available(self) -> bool:
        if self._found is None or not os.access(self._found, os.X_OK):
            self._found = shutil.which(self.aether_bin)
        return self._found is not None and self.data_dir.is_dir()
