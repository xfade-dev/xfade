class Xfade < Formula
  desc "Vendor-neutral, zero-raw-key gateway for AI coding CLIs"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.12.3/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "5cf6e7c8ca13ad6995aa8175d7e721b222ec9730aa9f06812edb02a574047bae"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.12.3/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "46f1fbc95fed101142fdc613938060215e0ab1ee271f626f503f85568907b0d2"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/v0.12.3/xfade-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "20186ae3e4ef4d62409d4d33e960286865a871d4a5fd300ee307eb55fefb89e0"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
