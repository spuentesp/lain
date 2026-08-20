# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.6.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.6.0/lain-0.6.0-aarch64-apple-darwin.tar.gz"
      sha256 "8c28d556f865b0e375d2a7f2bb8d2a81fd57c903548b73da4fbeac79d86026d1"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.6.0/lain-0.6.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d91554755d4df1a9360c122e577ff5c78c460673b964c5532a3b401a06b55c29"
    end
  end

  def install
    bin.install "lain"
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
