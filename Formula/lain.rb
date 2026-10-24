# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.11.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.11.0/lain-0.11.0-aarch64-apple-darwin.tar.gz"
      sha256 "b02ff20a58d9633302f2deb3ff40f2b9f535099d13d7b7759cc53c9e9172c857"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.11.0/lain-0.11.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e3487f2d7e2cfe1fa80b2e3f6f8816da8757d37c961590b97e8e2ed6de57258a"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.11.0/lain-0.11.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "ffc9c6a8af7e69a45b5f615962bc831bab63f7f18d24ce4c2529c40041575e79"
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
