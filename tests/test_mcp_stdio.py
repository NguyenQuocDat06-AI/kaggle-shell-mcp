"""End-to-end stdio MCP protocol verification (runs completely offline)."""

from __future__ import annotations

import sys

import pytest
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

EXPECTED_TOOLS = {
    "kaggle_up",
    "kaggle_status",
    "kaggle_down",
    "term_exec",
    "job_spawn",
    "job_list",
    "job_logs",
    "job_kill",
    "fs_sync",
}


@pytest.mark.asyncio
async def test_mcp_stdio_registers_all_nine_tools() -> None:
    params = StdioServerParameters(
        command=sys.executable,
        args=["-m", "kaggle_mcp.server"],
    )
    async with stdio_client(params) as (read_stream, write_stream):
        async with ClientSession(read_stream, write_stream) as session:
            await session.initialize()
            result = await session.list_tools()
            names = {t.name for t in result.tools}
            assert names == EXPECTED_TOOLS
            for tool in result.tools:
                assert tool.description and len(tool.description) > 15
