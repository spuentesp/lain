# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.6.1"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.6.1/lain-0.6.1-aarch64-apple-darwin.tar.gz"
      sha256 "5a93905af0fcabb0e3561177144c3001ee2d78bfad342406e46c391ae13b5e36"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.6.1/lain-0.6.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "dee3fe83f49ee585b2cdb0a365454fc31609d9d2caa90f4d61bd5824ce3e350b"
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
