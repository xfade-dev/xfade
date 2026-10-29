class Xfade < Formula
  desc "Route AI providers for AI coding tools, with local proxy"
  homepage "https://xfade.dev"
  version "0.9.0"
  license "MIT OR Apache-2.0"

  # This formula belongs to the standalone tap repo homebrew-xfade; the url/sha256 below are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.9.0/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "43e400cebea4e4afc240137b3fa1fdc5ff51b36c757bdfcb2a3b5f6d7d7d066b"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.9.0/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "ec40090a8cfbbc7b9b80e9f23a88abc8422cba45c15a102031c970e33d82dd78"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
