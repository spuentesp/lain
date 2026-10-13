# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.9.1"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.1/lain-0.9.1-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"  # POST-PUBLISH: fill from the v0.9.1 SHA256SUMS
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.1/lain-0.9.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"  # POST-PUBLISH: fill from the v0.9.1 SHA256SUMS
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.1/lain-0.9.1-x86_64-pc-windows-msvc.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"  # POST-PUBLISH: fill from the v0.9.1 SHA256SUMS
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
    EOS
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end
