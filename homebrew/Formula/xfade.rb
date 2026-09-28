class Xfade < Formula
  desc "Route AI providers for AI coding tools, with local proxy"
  homepage "https://xfade.dev"
  version "0.8.4"
  license "MIT OR Apache-2.0"

  # This formula belongs to the standalone tap repo homebrew-xfade; the url/sha256 below are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.8.4/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "85874b55e4dbe2130dca505a930bcc0664b4bfc3c2ca955ae997927fbda497ca"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.8.4/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "4bced15e0194bf2806342d25d917e792bdc8470ad1234807190d67e73306d8cb"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
