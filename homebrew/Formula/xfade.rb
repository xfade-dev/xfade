class Xfade < Formula
  desc "Route AI providers for AI coding tools, with local proxy"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.10.1/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "2b9255eff1cb281c51e6a0ba1b199d7d1ecea7fea929a1cfb4aaa3255dc09ef3"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.10.1/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "cb567782441ad14e308dd43802239e5cf4adda8c74289174dc6d0facf9c70562"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
