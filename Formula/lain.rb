# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.13.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.13.0/lain-0.13.0-aarch64-apple-darwin.tar.gz"
      sha256 "699685aa3e990412f74476d2bda6696a46bf6ac727d9caf1e8e8fd6e01003b0e"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.13.0/lain-0.13.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a30892c3162003f03cad4dc33d18d8ac5106a727215c11211524dd84cc9122fe"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.13.0/lain-0.13.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "c93792b4a08446c7b2b64948f621ded6439f1f5686dd4de8fb18cf5ae8817d49"
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
