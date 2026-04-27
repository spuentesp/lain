# LAIN e2e Tests

End-to-end tests for the LAIN MCP server.

## Setup

```bash
pip install -r requirements.txt
```

## Running

### Start the server first

```bash
# In one terminal:
cargo run -- --workspace /path/to/project --transport http --port 9999
```

### Run tests

```bash
# Option 1: Direct run
python tests/e2e/test_lain.py

# Option 2: With pytest
python -m pytest tests/e2e/ -v
```

### With custom binary/workspace

```bash
LAIN_BINARY=target/release/lain LAIN_WORKSPACE=/path/to/project python tests/e2e/test_lain.py
```

## Tests

- `test_health` - Server health check
- `test_sync_state` - Graph sync
- `test_get_tools` - Tool registry
- `test_explore_architecture` - Architecture exploration
- `test_list_entry_points` - Entry point detection
- `test_get_blast_radius` - Impact analysis
- `test_trace_dependency` - Dependency tracing
- `test_semantic_search` - Semantic search
- `test_get_file_diff` - Git diff
- `test_get_commit_history` - Git history
- `test_query_graph` - Graph query interface
- `test_confidence_field` - Confidence metadata
