# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.16.1"

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
      url "https://github.com/spuentesp/lain/releases/download/v0.16.1/lain-0.16.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5397b1fb9ca112f15fe670d2715d49d87f500503e5ae9834c3665b4c4a76883a"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.16.1/lain-0.16.1-x86_64-pc-windows-msvc.tar.gz"
      sha256 "b47d7967d28e6e8080e09bf626c148b3f13b98007c250241e795bc087ac620e6"
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
