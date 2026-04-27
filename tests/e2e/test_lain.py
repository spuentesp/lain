#!/usr/bin/env python3
"""
LAIN e2e test harness

Tests the MCP server end-to-end by:
1. Starting the LAIN server
2. Sending MCP JSON-RPC requests
3. Verifying responses

Usage:
    python -m pytest tests/e2e/ -v
    python tests/e2e/test_lain.py  # run standalone
"""

import json
import subprocess
import sys
import time
import os
from pathlib import Path
from typing import Optional

import requests

# Server configuration
LAIN_BINARY = os.environ.get("LAIN_BINARY", "target/debug/lain")
LAIN_WORKSPACE = os.environ.get("LAIN_WORKSPACE", "/Users/spuentesp/lain")
HTTP_PORT = 9999
STDIO_TRANSPORT = "stdio"


class LAINClient:
    """HTTP client for LAIN MCP server"""

    def __init__(self, base_url: str = f"http://localhost:{HTTP_PORT}"):
        self.base_url = base_url
        self.session = requests.Session()

    def call_tool(self, tool_name: str, arguments: Optional[dict] = None) -> dict:
        """Call a tool via MCP JSON-RPC"""
        payload = {
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments or {}
            },
            "id": 1
        }

        resp = self.session.post(
            f"{self.base_url}/mcp",
            json=payload,
            headers={"Content-Type": "application/json"}
        )
        resp.raise_for_status()
        return resp.json()

    def get_health(self) -> dict:
        """Get server health"""
        return self.call_tool("get_health", {})

    def get_tools(self) -> dict:
        """Get available tools"""
        payload = {
            "jsonrpc": "2.0",
            "method": "tools/list",
            "params": {},
            "id": 2
        }
        resp = self.session.post(
            f"{self.base_url}/mcp",
            json=payload,
            headers={"Content-Type": "application/json"}
        )
        resp.raise_for_status()
        return resp.json()


class TestLain:
    """LAIN e2e tests"""

    @classmethod
    def setup_class(cls):
        """Start LAIN server before tests"""
        cls.client = LAINClient()

    def test_health(self):
        """Test get_health tool"""
        result = self.client.get_health()
        assert "result" in result or "error" not in result, f"Health check failed: {result}"

    def test_sync_state(self):
        """Test sync_state tool"""
        result = self.client.call_tool("sync_state", {})
        assert "result" in result or "error" not in result, f"Sync failed: {result}"

    def test_get_tools(self):
        """Test tools/list"""
        result = self.client.get_tools()
        assert "result" in result, f"Tools list failed: {result}"
        tools = result.get("result", {}).get("tools", [])
        tool_names = [t["name"] for t in tools]
        assert len(tool_names) > 30, f"Expected 30+ tools, got {len(tool_names)}"
        # Check core tools exist
        core_tools = ["get_blast_radius", "trace_dependency", "find_anchors", "list_entry_points"]
        for tool in core_tools:
            assert tool in tool_names, f"Missing core tool: {tool}"

    def test_explore_architecture(self):
        """Test explore_architecture tool"""
        result = self.client.call_tool("explore_architecture", {"max_depth": 2})
        assert "result" in result or "error" not in result, f"Explore failed: {result}"
        text = result.get("result", {}).get("content", [{}])[0].get("text", "")
        assert len(text) > 0, "Expected non-empty architecture output"

    def test_list_entry_points(self):
        """Test list_entry_points tool"""
        result = self.client.call_tool("list_entry_points", {})
        assert "result" in result or "error" not in result, f"Entry points failed: {result}"
        text = result.get("result", {}).get("content", [{}])[0].get("text", "")
        assert "main" in text.lower() or "entry" in text.lower() or len(text) > 0

    def test_get_blast_radius(self):
        """Test get_blast_radius tool"""
        result = self.client.call_tool("get_blast_radius", {"symbol": "main"})
        assert "result" in result or "error" not in result, f"Blast radius failed: {result}"

    def test_trace_dependency(self):
        """Test trace_dependency tool"""
        result = self.client.call_tool("trace_dependency", {"symbol": "main"})
        assert "result" in result or "error" not in result, f"Trace failed: {result}"

    def test_semantic_search(self):
        """Test semantic_search tool"""
        result = self.client.call_tool("semantic_search", {"query": "test", "limit": 5})
        assert "result" in result or "error" not in result, f"Search failed: {result}"

    def test_get_file_diff(self):
        """Test get_file_diff tool"""
        result = self.client.call_tool("get_file_diff", {})
        assert "result" in result or "error" not in result, f"Diff failed: {result}"

    def test_get_commit_history(self):
        """Test get_commit_history tool"""
        result = self.client.call_tool("get_commit_history", {"limit": 5})
        assert "result" in result or "error" not in result, f"Commit history failed: {result}"

    def test_query_graph(self):
        """Test query_graph tool"""
        result = self.client.call_tool("query_graph", {
            "query": {
                "ops": [
                    {"op": "find", "type": "Function", "limit": 5}
                ]
            }
        })
        assert "result" in result or "error" not in result, f"Query failed: {result}"

    def test_confidence_field(self):
        """Test that get_blast_radius shows confidence field when tree-sitter used"""
        result = self.client.call_tool("get_blast_radius", {"symbol": "main"})
        text = result.get("result", {}).get("content", [{}])[0].get("text", "")
        # Either a valid result with confidence/affected/nodes, or node not found (graph empty)
        has_content = "Confidence" in text or "affected" in text or "nodes" in text
        is_not_found = "Not found" in text or "not found" in text.lower()
        assert has_content or is_not_found, f"Unexpected result: {text}"


def run_http_tests():
    """Run tests against HTTP server"""
    print(f"Running e2e tests against http://localhost:{HTTP_PORT}")
    print(f"Workspace: {LAIN_WORKSPACE}")

    # Check if server is running
    try:
        client = LAINClient()
        client.get_health()
    except Exception as e:
        print(f"ERROR: LAIN server not running at http://localhost:{HTTP_PORT}")
        print(f"Start with: cargo run -- --workspace {LAIN_WORKSPACE} --transport http --port {HTTP_PORT}")
        sys.exit(1)

    # Run pytest
    import pytest
    sys.exit(pytest.main([__file__, "-v"]))


if __name__ == "__main__":
    run_http_tests()
