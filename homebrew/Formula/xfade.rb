class Xfade < Formula
  desc "Vendor-neutral, zero-raw-key gateway for AI coding CLIs"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.13.2/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "bc344fc7c96d5d56c9cd738a72c9c7bdf5e036d50808281e4c505bd0e83554de"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.13.2/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "2c399a1cf8bdbb839bff88ef49232ef5e03dc4892952a49584d60a4868b1e5a1"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.13.2/xfade-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "bc28d73226ce244bf8519b385aa1a56d92b79140bf0957e6a2b05dcfeaf73dcb"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
