from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path


REPOSITORY = "kazuph/herdr"
SEMVER_COMPONENT = r"(?:0|[1-9][0-9]*)"
TAG_PATTERN = re.compile(rf"^kazuph-v({SEMVER_COMPONENT}\.{SEMVER_COMPONENT}\.{SEMVER_COMPONENT})$")


def version_for_release(repository: str, ref_name: str, cargo_version: str) -> str:
    if repository != REPOSITORY:
        raise ValueError(f"fork release requires repository {REPOSITORY}, got {repository}")
    match = TAG_PATTERN.fullmatch(ref_name)
    if match is None:
        raise ValueError(f"fork release tag must match kazuph-v<semver>, got {ref_name}")
    version = ref_name.removeprefix("kazuph-v")
    if version != match.group(1):
        raise ValueError(f"could not parse fork release version from {ref_name}")
    if cargo_version != version:
        raise ValueError(f"Cargo.toml version {cargo_version} does not match tag version {version}")
    return version


def cargo_version(manifest: Path) -> str:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--manifest-path", str(manifest)],
        check=True,
        capture_output=True,
        text=True,
    )
    packages = json.loads(result.stdout).get("packages", [])
    package = next((item for item in packages if Path(item["manifest_path"]).resolve() == manifest.resolve()), None)
    if package is None or not isinstance(package.get("version"), str):
        raise ValueError(f"cargo metadata has no package version for {manifest}")
    return package["version"]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", required=True)
    parser.add_argument("--ref-name", required=True)
    parser.add_argument("--cargo-manifest", type=Path, default=Path("Cargo.toml"))
    args = parser.parse_args()
    print(version_for_release(args.repository, args.ref_name, cargo_version(args.cargo_manifest)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
