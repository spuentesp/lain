# @spuentesp/lain-mcp

> Architectural code intelligence for AI coding agents.

## Installation

```bash
npm install -g @spuentesp/lain-mcp
```

If npm fails with `EEXIST: file already exists` for `lain`, an
unrelated `lain` file already sits in npm's global bin directory (often
an older manual copy). Move or remove it, or install under a different
prefix:

```bash
npm install -g --prefix ~/.local/lain-npm @spuentesp/lain-mcp
```

When npm install scripts are disabled (`--ignore-scripts`, a
locked-down CI), the binary is not downloaded at install time. The
first `lain` run fetches and verifies it, so that first run needs
network access; once cached, it works offline.

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

From a repository you want Lain to index, the real path is the setup
command — it writes this package's MCP registration into your agent's
configuration and verifies the connection:

```bash
lain setup --agent claude-code   # also: codex, cursor, vscode, continue, generic
```

`lain setup` registers the absolute path of the verified cache binary
(see "What this installs"), not the bare `lain` launcher — MCP hosts
rarely inherit the shell `PATH` that would resolve it. A hand-written
entry that calls the launcher works too when `PATH` reaches it:

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
