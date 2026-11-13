class Aibus < Formula
  desc "Terminal client that drives several LLM providers and agents"
  homepage "https://github.com/spdrman/aibus"
  version "0.2.0"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-apple-darwin.tar.gz"
      sha256 "e61799af362bcb922605002171a000ea1487d1b904d41c62050d0ffade2e5b46"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-apple-darwin.tar.gz"
      sha256 "6d4c0b995af34a81b2640f185f60505970d7f3f0abbb4c7f92c4f5df0477b10c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f505b667ddf2aac65d830c409d856aa5bc241ed8cf26929218b75cabb2c0c417"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-unknown-linux-musl.tar.gz"
      sha256 "afe6859958e215d1c96ed34ff6b9aa7337ca4f7d0ffdd17d6f277898979fb0c6"
    end
  end

  def install
    # Homebrew enters the archive's single top-level folder (aibus-<target>/) before this runs.
    bin.install "aibus"
    generate_completions_from_executable(bin/"aibus", "completions")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/aibus version")
  end
end
