# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.9.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.0/lain-0.9.0-aarch64-apple-darwin.tar.gz"
      sha256 "23c54ba1226d9b7411ae6b530b0628422790f893386664987ee1c2cbe2d6f555"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.0/lain-0.9.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d7dade8a57cc9b5c7e9ac94549ff7b97ad838a1ab8d4472f33844690e2429ef9"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.9.0/lain-0.9.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "73d28fba8a58e0519123fa28441377accecc25e77a867f383f2b9d1f3f11bfdf"
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
