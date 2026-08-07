# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://lain.dev"
  version "0.4.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.4.0/lain-0.4.0-aarch64-apple-darwin.tar.gz"
      sha256 "9ab21c41d2b3694a69f673e39fdbb9723d48d01547ac8d87f9166d6c01832163"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.4.0/lain-0.4.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "2499938b342a18fd9e01de4b0a598e69ca762d9e7bbe00bf629de95873bc1a5a"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.4.0/lain-0.4.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "24c2ef0b15ab07456e65d003ab7f3738c8a7eadebb0e8572f14e7d4af6bbeef5"
    end
  end

  def install
    bin.install "lain"
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end
