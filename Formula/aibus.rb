# PLACEHOLDER CHECKSUMS: the sha256 lines below are all zeros. The release workflow replaces
# this file with one rendered from the real SHA256SUMS (packaging/homebrew/render-formula.sh).
class Aibus < Formula
  desc "Terminal client that drives several LLM providers and agents"
  homepage "https://github.com/spdrman/aibus"
  version "0.1.0"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    # The archive unpacks to a folder named after the target.
    bin.install Dir["aibus-*/aibus"].first => "aibus"
    generate_completions_from_executable(bin/"aibus", "completions", shell_parameter_format: :arg)
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/aibus version")
  end
end
