# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://lain.dev"
  version "0.5.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.4.2/lain-0.4.2-aarch64-apple-darwin.tar.gz"
      sha256 "4d5ec84ed9540cade9ae56c10545d5ee9553dc4c7fb29f09b30e7fcb71c61539"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.4.2/lain-0.4.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b5f91be763a7fb2cfde52315d29e0b25bd6eefd3d928c9c32db2edf02f9c6e94"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.4.2/lain-0.4.2-x86_64-pc-windows-msvc.tar.gz"
      sha256 "409772c5517ed6d25a8ad631937746d48326b0129c75c3b8b39f5e83a2d47769"
    end
  end

  def install
    bin.install "lain"
  end

  def caveats
    <<~EOS
      Lain is installed. To run the MCP server for a project, point it at
      the project's repos.yaml:
            lain server --config ./repos.yaml

      For the full subcommand list (server, workspaces, repos, query, ask):
            lain --help
    EOS
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end