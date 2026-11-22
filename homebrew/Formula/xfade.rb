class Xfade < Formula
  desc "Vendor-neutral, zero-raw-key gateway for AI coding CLIs"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.12.1/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "deab745c31e686aa84c41067f06ff1f4cad74246e4998b0bfcba2450aa27af84"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.12.1/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "bc3ca8cfe02de0d83e1758b1a066b36a2dd615f48c51dabcebd1d46f3d34e314"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
