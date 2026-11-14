class Aibus < Formula
  desc "Terminal client that drives several LLM providers and agents"
  homepage "https://github.com/spdrman/aibus"
  version "0.2.1"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-apple-darwin.tar.gz"
      sha256 "ece8fe1a352bfb48b0f8da81580471b137c670908d64b4483ce541319ec9132e"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-apple-darwin.tar.gz"
      sha256 "ff446d3e943dee833709df046c8fca33b2d2dada3a8d1d1b09505510d3ca7950"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "33a5b036dd212085fb76d6d62b4268688b0443aeaee2bc5adad73a75ea675780"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-unknown-linux-musl.tar.gz"
      sha256 "315ddb75c80eb5d9bbe000d2cce4d86359f0762b567e5eda60ddbb177f982a7e"
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
