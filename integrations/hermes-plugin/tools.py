"""The ``aether`` toolset: send tasks to other Aether agents."""

from __future__ import annotations

import json
import os
import shutil
from functools import partial
from pathlib import Path
from typing import Any, Optional

from .mcp_client import McpError, call_tool
from .schemas import TOOLS


def _arguments(args: dict, properties: dict) -> dict:
    """Only the known arguments, without empty values, so the MCP server's
    strict argument check passes."""
    return {
        key: value
        for key, value in (args or {}).items()
        if key in properties and value is not None and value != ""
    }


def _handler(settings, mcp_tool: str, properties: dict, args: dict, **_: Any) -> str:
    try:
        is_error, text = call_tool(settings.aether_bin, settings.data_dir, mcp_tool, _arguments(args, properties))
    except McpError as error:
        return json.dumps({"error": str(error)})
    return json.dumps({"error": text}) if is_error else text


def register_tools(ctx, settings) -> None:
    for name, (mcp_tool, description, properties, required) in TOOLS.items():
        parameters: dict[str, Any] = {"type": "object", "properties": properties}
        if required:
            parameters["required"] = required
        ctx.register_tool(
            name=name,
            toolset="aether",
            schema={"name": name, "description": description, "parameters": parameters},
            handler=partial(_handler, settings, mcp_tool, properties),
            description=description,
            check_fn=settings.tools_available,
        )


class Settings:
    """The plugin settings: the node data directory and the ``aether`` command."""

    def __init__(self, data_dir: Path, aether_bin: str) -> None:
        self.data_dir = Path(data_dir).expanduser()
        self.aether_bin = aether_bin
        self._found: Optional[str] = None

    def tools_available(self) -> bool:
        # The path is found again when it was not found, or when that file is gone.
        if self._found is None or not os.access(self._found, os.X_OK):
            self._found = shutil.which(self.aether_bin)
        return self._found is not None and self.data_dir.is_dir()
