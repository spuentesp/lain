# Toolchains

Drop a file in here — or in `~/.lain/toolchains/` — and lain detects that
language. Filename is the toolchain name; a plain file's contents are the
marker to look for, or use TOML for the full profile.

```toml
name          = "rust"
marker        = "Cargo.toml"      # file whose presence identifies the project
priority      = 100
build_command = "cargo build --message-format=json"
test_command  = "cargo test --message-format=short"
build_parser  = "cargo-json"
test_parser   = "cargo-test"
```

## Finding the toolchain when it isn't on PATH

`run_build`, `run_tests` and `run_clippy` spawn the commands above. The
lain server inherits the environment of whatever launched it, and an
editor-launched MCP server routinely has **no version-manager shims on
`PATH`** — so the command that works in your terminal fails in lain with
`No such file or directory`.

Two optional fields tell lain where to look. Both are data, so teaching
lain about a manager it has never heard of means editing a file, not
patching Rust.

```toml
# Asked as `<program_resolver> <program>`; the path is read from stdout.
# For managers whose install directory is only knowable at runtime.
program_resolver = "rustup which"

# Searched in order when PATH comes up empty. `~` expands to your home
# directory, and a `*` component matches any directory at that level —
# which is the shape version managers actually install into.
program_dirs = [
  "~/.cargo/bin",
  "~/.rustup/toolchains/*/bin",
]
```

Resolution order for each program:

1. `$PROGRAM` — e.g. `$CARGO`, `$NPM`. An explicit override always wins.
2. `PATH`. If your environment already resolves it, that's your choice.
3. `program_resolver`, then `mise which` / `asdf which`. A manager
   reporting the *selected* toolchain beats guessing a version.
4. `program_dirs`, then `~/.local/share/mise/shims`, `~/.asdf/shims`,
   `~/.local/bin`. Directories matched by `*` are tried newest first.
5. The bare name — so the failure names the program you asked for
   rather than a path lain invented.

Shipped profiles already cover rustup, nvm, fnm, volta, bun, pnpm, yarn,
pyenv, rye, mise and asdf. Add to them per-project by dropping a TOML
file with the same `name` into `toolchains/`.
