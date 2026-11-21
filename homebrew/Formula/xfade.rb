class Xfade < Formula
  desc "Vendor-neutral, zero-raw-key gateway for AI coding CLIs"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.12.0/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "ce969bb74ad2625256c0ddbf2b02ecb5ad8422a1ed4095fb8db58e5eeb956f39"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.12.0/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "ef07c26f7f16d5b70f11b40a0bdbfc8b8861c6c0cf1d466fb1d241f45e7be6f7"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
