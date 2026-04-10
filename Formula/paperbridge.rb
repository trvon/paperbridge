class Paperbridge < Formula
  desc "MCP and CLI bridge for Zotero search and PDF/full-text retrieval"
  homepage "https://github.com/trvon/paperbridge"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/trvon/paperbridge/releases/download/v#{version}/paperbridge-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER"
    else
      url "https://github.com/trvon/paperbridge/releases/download/v#{version}/paperbridge-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER"
    end
  end

  on_linux do
    url "https://github.com/trvon/paperbridge/releases/download/v#{version}/paperbridge-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "PLACEHOLDER"
  end

  def install
    bin.install "paperbridge"
  end

  test do
    assert_match "paperbridge", shell_output("#{bin}/paperbridge --version")
  end
end
