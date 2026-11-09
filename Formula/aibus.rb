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
      sha256 "38de7561bde2f06351639c84492a31477b3f9685f5b87d113a849af24453530e"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-apple-darwin.tar.gz"
      sha256 "92a404960d1d05029c62c6675d8ddf466bb95eaf0a6aefbc43969165dc72a021"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "51c17d7991aefdbbc07567524f464eccc5b374b3d72a8846909cad57b79867ac"
    end
    on_intel do
      url "https://github.com/spdrman/aibus/releases/download/v#{version}/aibus-x86_64-unknown-linux-musl.tar.gz"
      sha256 "592d2ac01c8fdab549401f492de9ccffda70a6f966b7d137b7c083e350107686"
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
