# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.7.4-rc1"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.7.4-rc1/lain-0.7.4-rc1-aarch64-apple-darwin.tar.gz"
      sha256 "d207e7f6bbabf2f266eb61021315695f54c0039a0adeef63ba047bfdfeed7b9d"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.7.4-rc1/lain-0.7.4-rc1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4a0a34e7d0ee3cbe53299505450d718ba562ffed40e18ba955bae47b1a9679e0"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.7.4-rc1/lain-0.7.4-rc1-x86_64-pc-windows-msvc.tar.gz"
      sha256 "c5f1008d1f2e965d64d6b15658357d81ac4e19d71dc38937eba19fbeb3fc2e26"
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
