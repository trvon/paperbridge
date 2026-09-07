class Paperbridge < Formula
  desc "MCP and CLI bridge for Zotero search and PDF/full-text retrieval"
  homepage "https://github.com/trvon/paperbridge"
  version "2.0.0" # x-release-please-version
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/trvon/paperbridge/releases/download/v#{version}/paperbridge-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "2e907c5f983df2d084a581c1492c93ead3a155642330e5cf5e9ff92aff51aac2"
    else
      url "https://github.com/trvon/paperbridge/releases/download/v#{version}/paperbridge-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "1127b4127f9d82f3e125a8f0dda25c855f19ec63faa54f0a5a326d90f3cc38cd"
    end
  end

  on_linux do
    url "https://github.com/trvon/paperbridge/releases/download/v#{version}/paperbridge-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "d138c5c889b5f24cb8d24e5652e88678578e3b902d55ad22c4d390d9d190215a"
  end

  def install
    bin.install "paperbridge"
  end

  test do
    assert_match "paperbridge", shell_output("#{bin}/paperbridge --version")
  end
end
