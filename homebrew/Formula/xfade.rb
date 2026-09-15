class Xfade < Formula
  desc "Route AI providers for Claude Code / Codex / OpenCode / Pi / Oh My Pi / Aider, with local proxy"
  homepage "https://xfade.dev"
  version "0.7.0"
  license "MIT OR Apache-2.0"

  # This formula belongs to the standalone tap repo homebrew-xfade; the url/sha256 below are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.7.0/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.7.0/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_SHA256"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
