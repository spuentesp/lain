# typed: false
# frozen_string_literal: true

class Lain < Formula
  desc "Structural code intelligence for AI agents"
  homepage "https://lain.dev"
  version "0.3.1"

  on_macos do
    on_arm do
      url "https://github.com/spuentesp/lain/releases/download/v0.3.1/lain-0.3.1-aarch64-apple-darwin.tar.gz"
      sha256 "51c3474ef4a32ae288b2f1e1b53cac2142c75221b6e2c2605bdfc864d72bd839"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.3.1/lain-0.3.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "03ffbc49fe92b9e24adb2f5ae012fda1988748f9361eaec73808d5a1d28b386a"
    end
  end

  on_windows do
    on_intel do
      url "https://github.com/spuentesp/lain/releases/download/v0.3.1/lain-0.3.1-x86_64-pc-windows-msvc.tar.gz"
      sha256 "6f791e7f0ed58320df99283bd681fdcc84afa97b137dffaef746bd351405cafa"
    end
  end

  def install
    bin.install "lain"
  end

  test do
    assert_match "lain #{version}", shell_output("#{bin}/lain --version")
  end
end
