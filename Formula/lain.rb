# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://lain.dev"
  version "0.2.0"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.2.0/lain-0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "ee38fbde32a2908bc58c9d2a9d0b9751e305f22f46b94aa1ae41941b76319ab9"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.2.0/lain-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4e76d63994545d1305c95ea46b5d63da3daafc563bd7ce4458b238f7e4144b2e"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.2.0/lain-0.2.0-x86_64-pc-windows-msvc.tar.gz"
      sha256 "6138f21282510f571237301ecb8c2d995a3494ae470c23f544e539a6122645c7"
    end
  end

  def install
    bin.install "lain"
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end
