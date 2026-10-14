# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.10.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.10.0/lain-0.10.0-aarch64-apple-darwin.tar.gz"
      sha256 "caecd71e13c0a0e7c46d8d401d0e82d2ac78f50f397d883e8a5c1bff638011ae"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.10.0/lain-0.10.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "45d92393e06c3dcc5079cca7650feb6f31b20d1ada2221c575bb1f4a71df9263"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.10.0/lain-0.10.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "1a60b122076567c31e19682f96fe1ce1b59cf3074cad0d84506f0eeb25dd17e0"
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
