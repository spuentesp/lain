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
- A `lain --version` check that must match the npm package version.

The launcher downloads into a temporary directory and only moves verified
files into the versioned cache. It checks cached binaries again before running
them. A verified cache entry works offline.

## Published targets

| Host | Target | Current status |
|---|---|---|
| x86-64 Linux | `x86_64-unknown-linux-gnu` | Supported |
| Apple silicon | `aarch64-apple-darwin` | Supported |
| x86-64 Windows | `x86_64-pc-windows-msvc` | The repository contains the `DirectML.dll` packaging repair; `v0.7.4` predates it |

The release workflow doesn't currently publish x86-64 macOS or ARM64 Linux
archives. On those hosts, build Lain from source.

## Configuration

Set `LAIN_VERSION` to run a different published version. Set
`LAIN_CACHE_DIR` to override the operating system's user cache directory.

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
