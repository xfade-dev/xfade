class Xfade < Formula
  desc "Route AI providers for AI coding tools, with local proxy"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.11.1/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "5a647a4de51db3ba39af788eea2b0a25abb2a97937745ea1c5e13b2941aff254"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.11.1/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "12e492e23aaa386ce535ed5894587851c4b86a4bff598649361f31c2725b0a8c"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
