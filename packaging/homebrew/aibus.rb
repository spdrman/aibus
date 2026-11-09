# Template for the Homebrew tap formula (docs/RELEASE.md, "Homebrew and crates.io").
#
# The tap repository (spdrman/homebrew-aibus) is created by the owner, not from here. After a tag,
# copy this file to Formula/aibus.rb in the tap, set `version`, and replace each PLACEHOLDER_*
# checksum with the matching line of the release's SHA256SUMS. Nothing in this file has been
# installed on a clean machine yet.
class Aibus < Formula
  desc "Terminal client that drives several LLM providers and agents"
  homepage "https://github.com/spdrman/aibus"
  version "0.0.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_SHA256_AARCH64_APPLE_DARWIN"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_SHA256_X86_64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_SHA256_AARCH64_UNKNOWN_LINUX_GNU"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-unknown-linux-musl.tar.gz"
      sha256 "PLACEHOLDER_SHA256_X86_64_UNKNOWN_LINUX_MUSL"
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
