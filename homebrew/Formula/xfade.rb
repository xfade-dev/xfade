class Xfade < Formula
  desc "Route AI providers for Claude Code / Codex / OpenCode / Pi / Oh My Pi / Aider, with local proxy"
  homepage "https://xfade.dev"
  version "0.7.0"
  license "MIT OR Apache-2.0"

  # This formula belongs to the standalone tap repo homebrew-xfade; the url/sha256 below are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.7.0/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "b911b60fb1f81fad9909aa9465b9e82ddace5109b2bf6b67f02bacf2eafe8f28"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
