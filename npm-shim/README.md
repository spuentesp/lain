# @spuentesp/lain-mcp

> Architectural code intelligence for AI coding agents.

## Installation

```bash
npm install -g @spuentesp/lain-mcp
```

Or with npx (downloads binary on first run):
```bash
npx @spuentesp/lain-mcp mcp
```

## What this installs

- A native binary downloaded from the matching GitHub release.
- A cache entry separated by LAIN version and platform target.
- SHA-256 verification against the release's `SHA256SUMS` file.

## Configuration

Set `LAIN_VERSION` to run a different published version. Set
`LAIN_CACHE_DIR` to override the operating system's user cache directory.
An already verified cache entry works offline.

## Usage

After installation, add Lain to your MCP configuration:

```json
{
  "mcpServers": {
    "lain": {
      "command": "lain",
      "args": ["mcp"]
    }
  }
}
```

Or run directly:

```bash
npx @spuentesp/lain-mcp mcp
```

## Uninstall

```bash
npm uninstall -g @spuentesp/lain-mcp
```

The verified native binary remains in your user cache so a later install can
work offline. Remove the `lain` cache directory using your operating system's
cache-management tools if you also want to discard downloaded versions.
