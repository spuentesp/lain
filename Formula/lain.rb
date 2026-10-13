# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.9.1"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.1/lain-0.9.1-aarch64-apple-darwin.tar.gz"
      sha256 "649f0338bcd7d002fccd35f4e20ad51cbdb71f8b42187a45f893b2f5a4c768fb"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.1/lain-0.9.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "28fcc99147cbb6bd6fa007a2643c543b97e066543ec7a7ecc99baa35eb6dcf9a"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.1/lain-0.9.1-x86_64-pc-windows-msvc.tar.gz"
      sha256 "db59d4f605a9a8e1be075d5d219effa660a81d9a6d2bce3e239721471bda607e"
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
