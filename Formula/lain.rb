# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.7.3"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.7.3/lain-0.7.3-aarch64-apple-darwin.tar.gz"
      sha256 "f5c85c42a4fbda36042bbe9b16c19c658a551cdec3f0232092e23429fec91697"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.7.3/lain-0.7.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "53867993e8f2e7a5066bc33d0ef5e4efcc72d9886c4796ecaf6b34636bcf165d"
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
