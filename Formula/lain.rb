# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://lain.dev"
  version "0.2.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.2.0/lain-0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "UPDATE_SHA256_ARM_MAC"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.2.0/lain-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "UPDATE_SHA256_INTEL_LINUX"
    end

    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.2.0/lain-0.2.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "UPDATE_SHA256_ARM_LINUX"
    end
  end

  def install
    bin.install "lain"
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end