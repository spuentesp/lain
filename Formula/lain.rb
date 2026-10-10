# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.8.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.8.0/lain-0.8.0-aarch64-apple-darwin.tar.gz"
      sha256 "1f55f308c1de6a9c941d8e9e95bb7cc95be0f33eb2aa7ed0c555714cbb2c0e83"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.8.0/lain-0.8.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "72dedbbf5f80297530262f2e6be5f0964d9f9e7cfd113199d3b298bd1a9bb6e8"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.8.0/lain-0.8.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "47dc5d37922d91463d8fae0bcd4086515d2e0c4f07ce71f5964934b6bdfff097"
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
