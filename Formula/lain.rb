# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://github.com/spuentesp/lain"
  version "0.13.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.13.0/lain-0.13.0-aarch64-apple-darwin.tar.gz"
      sha256 "11dd9e00fe134322490e31397995962d8dcaa7a0ced303437d30948041675cd0"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.13.0/lain-0.13.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "901a393c6e7fb4cd42f838ec0d25189b2f979ae85f9a86c55f7d585361940f8b"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.13.0/lain-0.13.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "16687aaa81acc039b5293d53d20fddabae45dad381b75e9863883e125977d87e"
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
