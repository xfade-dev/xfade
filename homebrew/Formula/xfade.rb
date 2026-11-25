class Xfade < Formula
  desc "Vendor-neutral, zero-raw-key gateway for AI coding CLIs"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.13.1/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "1c83874a4b1a869ee193dc7e0334817f6219d0bc3ba833dc19f934d985b9e7b5"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.13.1/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "91c82bc00bbd89d1e99ed15455ae3c054123e9653a68eabf6907662ee2cf3b75"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.13.1/xfade-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "cb188065afc67c27ea93be3686608fea753be71dcb36ab783aaf5b19d46c98c9"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
