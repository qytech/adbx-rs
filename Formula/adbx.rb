class Adbx < Formula
  desc "Enhanced cross-platform ADB with session locking, broadcast, TUI logcat & MCP"
  homepage "https://github.com/Jax-Lifu/adbx-rs"
  version "0.0.1"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jax-Lifu/adbx-rs/releases/download/v0.0.1/adbx-v0.0.1-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_MACOS_ARM64_SHA256"
    else
      url "https://github.com/Jax-Lifu/adbx-rs/releases/download/v0.0.1/adbx-v0.0.1-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_MACOS_X86_64_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jax-Lifu/adbx-rs/releases/download/v0.0.1/adbx-v0.0.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_LINUX_ARM64_SHA256"
    else
      url "https://github.com/Jax-Lifu/adbx-rs/releases/download/v0.0.1/adbx-v0.0.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_LINUX_X86_64_SHA256"
    end
  end

  def install
    bin.install "adbx"
  end

  def caveats
    <<~EOS
      Enable dynamic shell completion (including remote device paths):
        Zsh:  source <(COMPLETE=zsh adbx)
        Bash: source <(COMPLETE=bash adbx)
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/adbx --version")
  end
end
