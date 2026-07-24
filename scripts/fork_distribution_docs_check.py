from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

CURRENT_GUIDANCE = (
    "README.md",
    "docs/next/README.md",
    "website/index.html",
    "website/agent-guide.md",
    "website/src/pages/compare.astro",
    "website/src/content/docs/install.mdx",
    "website/src/content/docs/ja/install.mdx",
    "website/src/content/docs/zh-cn/install.mdx",
    "docs/next/website/src/content/docs/install.mdx",
    "docs/next/website/src/content/docs/ja/install.mdx",
    "docs/next/website/src/content/docs/zh-cn/install.mdx",
)

FORBIDDEN = (
    "curl -fsSL https://herdr.dev/install.sh",
    "irm https://herdr.dev/install.ps1",
    "brew install herdr",
    "nix run github:ogulcancelik/herdr",
    "https://github.com/ogulcancelik/herdr/releases",
    "https://github.com/ogulcancelik/herdr",
)

REQUIRED = {
    "README.md": (
        "git clone https://github.com/kazuph/herdr",
        "https://github.com/kazuph/herdr/releases",
    ),
    "website/index.html": (
        "git clone https://github.com/kazuph/herdr",
        "https://github.com/kazuph/herdr/releases",
    ),
    "website/agent-guide.md": (
        "git clone https://github.com/kazuph/herdr",
        "https://github.com/kazuph/herdr/releases",
    ),
}


def main() -> int:
    errors: list[str] = []
    contents: dict[str, str] = {}

    for relative in CURRENT_GUIDANCE:
        path = ROOT / relative
        if not path.is_file():
            errors.append(f"{relative}: current guidance file is missing")
            continue
        text = path.read_text(encoding="utf-8")
        contents[relative] = text
        for forbidden in FORBIDDEN:
            if forbidden in text:
                errors.append(f"{relative}: forbidden distribution path: {forbidden}")

    for relative, required_values in REQUIRED.items():
        text = contents.get(relative, "")
        for required in required_values:
            if required not in text:
                errors.append(f"{relative}: required fork distribution path is missing: {required}")

    if errors:
        for error in errors:
            print(f"error: {error}")
        return 1

    print(f"fork distribution docs: ok ({len(CURRENT_GUIDANCE)} current guidance files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
