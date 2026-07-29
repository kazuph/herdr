from __future__ import annotations

import argparse
import re
from pathlib import Path


ASSETS = (
    "herdr-linux-x86_64",
    "herdr-linux-aarch64",
    "herdr-macos-x86_64",
    "herdr-macos-aarch64",
)
VERSION_PATTERN = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
HASH_PATTERN = re.compile(r"^[0-9a-f]{64}$")


def checksums_from(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if len(parts) != 2:
            raise ValueError(f"invalid checksum line: {line}")
        digest, asset = parts
        if asset not in ASSETS or not HASH_PATTERN.fullmatch(digest):
            raise ValueError(f"invalid checksum entry: {line}")
        if asset in values:
            raise ValueError(f"duplicate checksum for {asset}")
        values[asset] = digest
    if set(values) != set(ASSETS):
        raise ValueError("checksums must contain the four fork release assets")
    return values


def formula(version: str, checksums: dict[str, str]) -> str:
    if not VERSION_PATTERN.fullmatch(version):
        raise ValueError(f"invalid version: {version}")

    def source(asset: str) -> str:
        return f'https://github.com/kazuph/herdr/releases/download/kazuph-v{version}/{asset}'

    return f'''class Herdr < Formula
  desc "Terminal workspace manager for AI coding agents"
  homepage "https://github.com/kazuph/herdr"
  version "{version}"
  license "AGPL-3.0-or-later"

  on_macos do
    if Hardware::CPU.arm?
      url "{source("herdr-macos-aarch64")}"
      sha256 "{checksums["herdr-macos-aarch64"]}"
    else
      url "{source("herdr-macos-x86_64")}"
      sha256 "{checksums["herdr-macos-x86_64"]}"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "{source("herdr-linux-aarch64")}"
      sha256 "{checksums["herdr-linux-aarch64"]}"
    else
      url "{source("herdr-linux-x86_64")}"
      sha256 "{checksums["herdr-linux-x86_64"]}"
    end
  end

  def install
    artifact = Dir["herdr-*"].first
    chmod 0755, artifact
    bin.install artifact => "herdr"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/herdr --version")
  end
end
'''


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--checksums", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(formula(args.version, checksums_from(args.checksums)), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
