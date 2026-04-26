# Lain

*Structural Memory & Architectural Brain for AI Agents*

---

## The Microscope vs. The Telescope

Current AI coding agents rely on standard Language Server Protocols (LSP) or brute-force RAG. This approach excels at microscopic tasks (fixing a line, writing a function) but fails at macroscopic tasks (understanding system-wide impact, architectural refactoring, navigating massive decoupling).

**Lain** provides the "telescope." It builds a persistent, queryable knowledge graph of your entire project, combining:
- **Structural Memory**: Every class, function, and relationship stored in a high-performance graph.
- **Architectural Reasoning**: Identify "anchors" (central symbols) and "blast radius" for any change.
- **Git Intelligence**: Understand which files change together (co-change analysis) and track knowledge staleness.
- **Local NLP**: Semantic search over code symbols using high-performance ONNX embeddings (no API keys required).

---

## Core Technologies
Lain is built for high-performance and local-first reliability:
- **KùzuDB**: Native, persistent graph database for sub-millisecond relationship queries.
- **Git2**: Embedded git integration for change detection and history analysis.
- **LSP Bridge**: Multi-language support via standard Language Server Protocols.
- **ORT (ONNX Runtime)**: Local inference for `all-MiniLM-L6-v2` embeddings.

---

## Installation

Lain requires a Linux-compatible environment (Debian/Ubuntu) for KùzuDB stability. Docker is the recommended path for all platforms.

### Option A: Docker (Recommended)
Add Lain to your MCP configuration (e.g. `claude_desktop_config.json`):
```json
{
  "mcpServers": {
    "lain": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-v", "/your/project/path:/workspace",
        "ghcr.io/spuentesp/lain:latest",
        "--workspace", "/workspace"
      ]
    }
  }
}
```

### Option B: Manual Build (Linux / WSL)
1. **Prerequisites**: Ensure `cmake`, `g++`, `pkg-config`, and `libssl-dev` are installed.
2. **Clone & Build**:
```bash
git clone https://github.com/spuentesp/lain.git
cd lain
cargo build --release
```
3. **Run**:
```bash
./target/release/lain --workspace /path/to/your/project
```

---

## Tools provided by Lain

Lain exposes 22 high-value tools to AI agents:

### 1. Architecture Exploration
- `get_layered_map`: Returns symbols at a specific depth from entry points.
- `get_master_map`: High-level summary of the entire system architecture.
- `get_entry_points`: Identifies `main` functions and root modules.
- `find_anchors`: Lists the most "central" symbols in the codebase.

### 2. Search & Navigation
- `search_symbols`: Fast string-based search over symbols.
- `semantic_search`: Intent-based search using local ONNX embeddings.
- `get_node_info`: Detailed metadata for a specific code symbol.
- `get_outgoing_edges`: Explore relationships (Calls, Uses, Inherits) for a symbol.

### 3. Impact & Health
- `get_blast_radius`: Identifies all symbols affected by changing a specific symbol.
- `get_call_chain`: Traces execution paths between symbols.
- `get_coupling_report`: Shows which files are logically coupled based on git history.
- `get_health`: Current state of the graph (nodes, edges, staleness).

---

## How to use Lain as an AI Agent

Lain is designed to be the primary interface for coding agents. If you are an agent, call the `get_agent_strategy` tool to receive your operational manual.

### Recommended Agent Flow:
1. **Orient**: Call `get_master_map` to see the big picture.
2. **Explore**: Call `find_anchors` to find the heart of the system.
3. **Trace**: Call `get_call_chain` to understand how a feature is triggered.
4. **Safety**: Call `get_blast_radius` before making any major refactor.

---

## License
MIT - Copyright (c) 2026 spuentesp
