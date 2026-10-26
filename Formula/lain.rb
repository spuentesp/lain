# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.12.1"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.12.1/lain-0.12.1-aarch64-apple-darwin.tar.gz"
      sha256 "28d4ef8065f6bcea43d88b02b141db265dc54f924ca3ba27245bdd838747fa7c"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.12.1/lain-0.12.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5fab4b69e0a829fc7299748fa96eacbdab8a8d41fb0f47a7c298c74a1b5e8b82"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.12.1/lain-0.12.1-x86_64-pc-windows-msvc.tar.gz"
      sha256 "e88d0d5596b8c53120f2da820307d203dcb0566a7dc68ed3ac0b7fa78c047def"
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
