class Xfade < Formula
  desc "Route AI providers for Claude Code / Codex / OpenCode / Pi / Oh My Pi / Aider, with local proxy"
  homepage "https://xfade.dev"
  version "0.7.0"
  license "MIT OR Apache-2.0"

  # This formula belongs to the standalone tap repo homebrew-xfade; the url/sha256 below are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.7.0/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "da0dd5cee87fc6a3852fed4310ce455e3898137ffc944bf547a9ae539bf48581"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.7.0/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "d4b7761aeed5c9077fc4b6ff4ff1c52818a53c36b03ae8bb9658041d1517e0a0"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
