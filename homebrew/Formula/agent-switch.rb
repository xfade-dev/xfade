class AgentSwitch < Formula
  desc "Switch API providers for Claude Code / Codex / OpenCode, with local proxy"
  homepage "https://github.com/<user>/agent-switch"
  version "0.6.0"
  license "MIT OR Apache-2.0"

  # 该 formula 属独立 tap 仓库 homebrew-agent-switch；以下 url/sha256 在每次 release 后更新。
  on_macos do
    on_arm do
      url "https://github.com/<user>/agent-switch/releases/download/v0.6.0/asw-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/<user>/agent-switch/releases/download/v0.6.0/asw-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_SHA256"
    end
  end

  def install
    bin.install "asw"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/asw --version")
  end
end
