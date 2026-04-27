# lain-mcp

> Architectural code intelligence for AI coding agents.

## Installation

```bash
npm install -g lain-mcp
```

Or with npx (downloads binary on first run):
```bash
npx lain-mcp --workspace /path/to/project
```

## What this installs

- **Binary**: `~/.lain/bin/lain` — the Lain executable (downloaded from GitHub releases)
- **Config**: `~/.lain/tuning.toml` — default tuning parameters
- **Toolchains**: `~/.lain/toolchains/` — empty dir for user toolchain overrides
- **Models**: `~/.lain/models/` — empty dir for ONNX embedding models (optional)

## Configuration

Edit `~/.lain/tuning.toml` to customize Lain's behavior. All values are optional — omit a field to use the built-in default.

## Usage

After installation, add Lain to your MCP configuration:

```json
{
  "mcpServers": {
    "lain": {
      "command": "lain",
      "args": ["--workspace", "/path/to/your/project"]
    }
  }
}
```

Or run directly:

```bash
~/.lain/bin/lain --workspace /path/to/project --transport stdio
```

## Uninstall

```bash
npm uninstall -g lain-mcp
# Binary and config remain in ~/.lain/ — delete manually to fully remove
rm -rf ~/.lain
```
