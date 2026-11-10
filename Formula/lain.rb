# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.15.0"

  # No `on_macos` stanza: `release.yml` builds Linux and Windows only.
  # The macOS ARM64 target was dropped in v0.14.0 (an unresolvable ONNX
  # Runtime linker error) and has not returned, so a macOS stanza would
  # point at an asset that is never published — `brew install` would
  # fail on a 404 instead of on a checksum.
  #
  # The `sha256` values below are placeholders until the release is
  # published: the artifacts are not bit-reproducible, so the real
  # hashes cannot be known before `release.yml` uploads them. They are
  # filled from the published `SHA256SUMS` in the post-publish commit
  # (see `scripts/fill-formula-sha256.py` and AGENTS.md step 6).
  #
  # `brew audit` / a tap PR must not be cut from this state: a
  # placeholder hash fails verification. Wait for the post-publish
  # fill.

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.15.0/lain-0.15.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.15.0/lain-0.15.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "lain"
    bin.install "lain-git-sidecar" if File.exist?("lain-git-sidecar")
  end

  def caveats
    <<~EOS
      Lain is installed. The zero-config MCP entry point is:
            lain mcp
      (run from anywhere inside a git repo — no repos.yaml needed).

      For the full subcommand list (server, mcp, workspaces, repos, query, hooks, doctor):
            lain --help

      Semantic search needs an embedding model, which Homebrew does not fetch.
      Install the default one once:
            lain models install
    EOS
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end
