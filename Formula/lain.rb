# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://lain.dev"
  version "0.3.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.3.0/lain-0.3.0-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.3.0/lain-0.3.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.3.0/lain-0.3.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "PLACEHOLDER"
    end
  end

  def install
    bin.install "lain"
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end
