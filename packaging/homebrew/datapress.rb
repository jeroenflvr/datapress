# Homebrew formula for the prebuilt `datapress` CLI.
#
# This installs the standalone binary (both DuckDB + DataFusion backends
# bundled) from the GitHub release — it does NOT compile from source, so it
# avoids the heavy bundled DuckDB C++ build.
#
# It belongs in a Homebrew *tap*, not homebrew-core. To publish:
#
#   1. Create a repo named `homebrew-tap` under your account/org, e.g.
#      https://github.com/jeroenflvr/homebrew-tap
#   2. Put this file at `Formula/datapress.rb` in that repo.
#   3. Users then run:
#        brew tap jeroenflvr/tap
#        brew install datapress
#
# The `update-homebrew` job in .github/workflows/publish.yml bumps the
# version + sha256 values automatically on each `v*` release (see
# packaging/README.md for the required token).
class Datapress < Formula
  desc "Fast multi-backend (DuckDB / DataFusion) HTTP server over Parquet and Delta"
  homepage "https://datap-rs.org"
  version "0.4.4"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/jeroenflvr/datapress/releases/download/v#{version}/datapress-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "c313e8347e681236b0320bf2c25cdb45502961bad352193edfa8aeae42bf1af0"
    end
    on_intel do
      odie "datapress has no prebuilt Intel macOS binary. Install with: cargo install datapress"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/jeroenflvr/datapress/releases/download/v#{version}/datapress-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "d1245dd374c62b44c617c4b6d6091396170aee51f65dd2336f4f558b0b6ab660"
    end
    on_intel do
      url "https://github.com/jeroenflvr/datapress/releases/download/v#{version}/datapress-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "ebe497d5744eecb9221232ebdaa8dfb2c17c3f8f3d18ed363636580c287c1bb6"
    end
  end

  def install
    bin.install "datapress"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/datapress --version")
  end
end
