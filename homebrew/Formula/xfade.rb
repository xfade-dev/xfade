class Xfade < Formula
  desc "Route AI providers for AI coding tools, with local proxy"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.9.1/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "40df65a9594dfe95f08adf34424c3f6e44f3d988a35dc43c78e991fd9849d2b8"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.9.1/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "352a1d8225c064c0ce3a0e6cbee55718cade4c65710b59b96def2cd7c15f8f74"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
