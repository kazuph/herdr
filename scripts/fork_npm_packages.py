from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
import platform
import shutil
import subprocess
import tarfile
import tempfile
import threading
import urllib.parse
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator


ROOT = Path(__file__).resolve().parents[1]
PACKAGES = {
    "herdr-darwin-arm64": "herdr-macos-aarch64",
    "herdr-darwin-x64": "herdr-macos-x86_64",
    "herdr-linux-arm64": "herdr-linux-aarch64",
    "herdr-linux-x64": "herdr-linux-x86_64",
}
WRAPPER = "herdr"


def package_directory(name: str) -> Path:
    return ROOT / "npm" / "packages" / name


def package_json(name: str) -> dict[str, object]:
    return json.loads((package_directory(name) / "package.json").read_text(encoding="utf-8"))


def expected_host_package() -> str:
    os_name = platform.system().lower()
    machine = platform.machine().lower()
    machine = {"aarch64": "arm64", "x86_64": "x64", "amd64": "x64"}.get(machine, machine)
    name = f"herdr-{os_name}-{machine}"
    if name not in PACKAGES:
        raise ValueError(f"unsupported host platform {os_name}/{machine}")
    return name


def assert_package_contracts(version: str) -> None:
    wrapper = package_json(WRAPPER)
    if wrapper.get("version") != version or wrapper.get("scripts"):
        raise ValueError("wrapper version must match Cargo and wrapper must not define lifecycle scripts")
    if wrapper.get("repository") != "https://github.com/kazuph/herdr":
        raise ValueError("wrapper must publish the fork repository provenance")
    dependencies = wrapper.get("optionalDependencies")
    expected = {f"@kazuph/{name}": version for name in PACKAGES}
    if dependencies != expected:
        raise ValueError("wrapper optionalDependencies must list the four exact-version native packages")
    for name, asset in PACKAGES.items():
        package = package_json(name)
        if package.get("version") != version or package.get("scripts"):
            raise ValueError(f"{name} must match Cargo and must not define lifecycle scripts")
        os_name, cpu = name.removeprefix("herdr-").split("-", 1)
        if package.get("os") != [os_name] or package.get("cpu") != [cpu]:
            raise ValueError(f"{name} OS/CPU metadata is not exact")
        if package.get("files") != ["bin/herdr"]:
            raise ValueError(f"{name} must package only bin/herdr")
        if package.get("repository") != "https://github.com/kazuph/herdr":
            raise ValueError(f"{name} must publish the fork repository provenance")
        if "libc" in package:
            raise ValueError(f"{name} must not exclude compatible Linux libc hosts")
        if not (Path(asset).name == asset):
            raise ValueError(f"invalid asset {asset}")


def packed_file(path: Path) -> Path:
    result = subprocess.run(
        ["npm", "pack", "--json", "--pack-destination", str(path.parent)],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    )
    return path.parent / json.loads(result.stdout)[0]["filename"]


def verify_tarball(path: Path, binary: Path | None) -> None:
    with tarfile.open(path, "r:gz") as archive:
        names = sorted(member.name for member in archive.getmembers() if member.isfile())
        expected = ["package/bin/herdr", "package/package.json"]
        if names != expected:
            raise ValueError(f"{path.name} contains unexpected files: {names}")
        launcher = archive.getmember("package/bin/herdr")
        if launcher.mode & 0o111 == 0:
            raise ValueError(f"{path.name} launcher is not executable")
        if binary is not None:
            packaged = archive.extractfile("package/bin/herdr")
            if packaged is None or hashlib.sha256(packaged.read()).digest() != hashlib.sha256(binary.read_bytes()).digest():
                raise ValueError(f"{path.name} does not contain the exact CI-built binary")


def pack(artifacts: Path, output: Path, version: str) -> None:
    assert_package_contracts(version)
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as temp:
        staging = Path(temp)
        for name, asset_name in PACKAGES.items():
            artifact = artifacts / asset_name
            if not artifact.is_file():
                raise ValueError(f"missing CI-built artifact: {asset_name}")
            target = staging / name
            shutil.copytree(package_directory(name), target)
            binary = target / "bin" / "herdr"
            binary.parent.mkdir()
            shutil.copy2(artifact, binary)
            binary.chmod(0o755)
            tarball = packed_file(target)
            verify_tarball(tarball, artifact)
            shutil.move(tarball, output / tarball.name)
        target = staging / WRAPPER
        shutil.copytree(package_directory(WRAPPER), target)
        tarball = packed_file(target)
        verify_tarball(tarball, None)
        shutil.move(tarball, output / tarball.name)


def tarball(output: Path, name: str, version: str) -> Path:
    path = output / f"kazuph-{name}-{version}.tgz"
    if not path.is_file():
        raise ValueError(f"missing packed package: {path.name}")
    return path


def run(command: list[str], cwd: Path, expected: str, failure: bool = False, env: dict[str, str] | None = None) -> None:
    result = subprocess.run(command, cwd=cwd, capture_output=True, text=True, env=env)
    output = result.stdout + result.stderr
    if failure:
        if result.returncode == 0 or expected not in output:
            raise ValueError(f"expected failure from {' '.join(command)}: {output}")
    elif result.returncode != 0 or expected not in output:
        raise ValueError(f"command failed {' '.join(command)}: {output}")


@contextmanager
def local_registry(output: Path, version: str) -> Iterator[str]:
    archives = {
        f"@kazuph/{name}": tarball(output, name, version)
        for name in (*PACKAGES, WRAPPER)
    }

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            decoded = urllib.parse.unquote(urllib.parse.urlparse(self.path).path)
            if decoded.startswith("/tarballs/"):
                archive = next((path for path in archives.values() if path.name == decoded.removeprefix("/tarballs/")), None)
                if archive is None:
                    self.send_error(404)
                    return
                body = archive.read_bytes()
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return

            name = decoded.removeprefix("/")
            archive = archives.get(name)
            if archive is None:
                self.send_error(404)
                return
            with tarfile.open(archive, "r:gz") as package_archive:
                package_file = package_archive.extractfile("package/package.json")
                if package_file is None:
                    self.send_error(500)
                    return
                metadata = json.loads(package_file.read())
            digest = hashlib.sha1(archive.read_bytes()).hexdigest()
            metadata["dist"] = {
                "shasum": digest,
                "tarball": f"http://127.0.0.1:{self.server.server_port}/tarballs/{archive.name}",
            }
            body = json.dumps(
                {
                    "_id": name,
                    "name": name,
                    "dist-tags": {"latest": version},
                    "versions": {version: metadata},
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format: str, *args: object) -> None:
            return

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


def test(output: Path, version: str) -> None:
    assert_package_contracts(version)
    host = expected_host_package()
    with tempfile.TemporaryDirectory() as temp:
        dependencies = {"@kazuph/herdr": version}
        with local_registry(output, version) as registry:
            npm_project = Path(temp) / "npm"
            npm_project.mkdir()
            (npm_project / "package.json").write_text(json.dumps({"private": True, "dependencies": dependencies}), encoding="utf-8")
            run(
                ["npm", "install", "--ignore-scripts", "--registry", registry, "--cache", str(Path(temp) / "npm-cache")],
                npm_project,
                "added",
            )
            installed = [name for name in PACKAGES if (npm_project / "node_modules" / "@kazuph" / name).exists()]
            if installed != [host]:
                raise ValueError(f"expected exactly one installed native package, got {installed}")
            run(["npx", "--no-install", "herdr", "--version"], npm_project, version)

            bun_project = Path(temp) / "bun"
            bun_project.mkdir()
            (bun_project / "package.json").write_text(json.dumps({"private": True, "dependencies": dependencies}), encoding="utf-8")
            bun_env = {**os.environ, "BUN_INSTALL_CACHE_DIR": str(Path(temp) / "bun-cache")}
            run(["bun", "install", "--ignore-scripts", "--registry", registry], bun_project, "installed", env=bun_env)
            bun_installed = [name for name in PACKAGES if (bun_project / "node_modules" / "@kazuph" / name).exists()]
            if bun_installed != [host]:
                raise ValueError(f"expected exactly one Bun-installed native package, got {bun_installed}")
            run(["bunx", "--no-install", "herdr", "--version"], bun_project, version, env=bun_env)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("pack", "test"):
        subparser = subparsers.add_parser(command)
        subparser.add_argument("--output", type=Path, required=True)
        if command == "pack":
            subparser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "pack":
        pack(args.artifacts, args.output, args.version)
    else:
        test(args.output, args.version)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
