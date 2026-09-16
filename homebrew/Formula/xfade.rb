class Xfade < Formula
  desc "Route AI providers for Claude Code / Codex / OpenCode / Pi / Oh My Pi / Aider, with local proxy"
  homepage "https://xfade.dev"
  version "0.7.0"
  license "MIT OR Apache-2.0"

  # This formula belongs to the standalone tap repo homebrew-xfade; the url/sha256 below are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.7.0/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "6e339dbcd2126dab7d7ba4a9b7ce263edf9295ac69d3073132d17a18c2aece23"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.7.0/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "e6d745253b58edf0c0a24585454411ca422d78d15cd4b70d3e6624033f7852d2"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
