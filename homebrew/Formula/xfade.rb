class Xfade < Formula
  desc "Route AI providers for AI coding tools, with local proxy"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.10.0/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "04545777e266102bf185a0f427d40fdb20f4fa56a94ad74ed026ab8aa0724d83"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.10.0/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "227e59d5ca8eda9a743ebe46ddfb5bee285a84e50873d93ee7ee91bd1812780d"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
