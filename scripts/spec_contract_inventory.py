from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DOMAIN_PATTERN = re.compile(r"^## (G\d+)\.\s+(.+)$")
SECTION_PATTERN = re.compile(r"^###\s+(.+)$")


def collect_contracts(spec_path: Path) -> list[dict[str, Any]]:
    lines = spec_path.read_text(encoding="utf-8").splitlines()
    domain = ""
    domain_title = ""
    section = ""
    section_retired = False
    collecting = False
    contracts: list[dict[str, Any]] = []

    for line_number, line in enumerate(lines, start=1):
        domain_match = DOMAIN_PATTERN.match(line)
        if domain_match:
            domain, domain_title = domain_match.groups()
            section = ""
            section_retired = False
            collecting = False
            continue

        section_match = SECTION_PATTERN.match(line)
        if section_match:
            section = section_match.group(1)
            section_retired = False
            collecting = False
            continue

        if line.startswith("- **status:"):
            section_retired = "破棄" in line
            continue

        if line == "- **受け入れ条件**:":
            collecting = True
            continue

        if collecting and line.startswith("  - "):
            text = line[4:]
            identity = "\n".join((domain, section, text)).encode()
            digest = hashlib.sha256(identity).hexdigest()[:10]
            contracts.append(
                {
                    "id": f"{domain}-{digest}",
                    "domain": domain,
                    "domain_title": domain_title,
                    "section": section,
                    "line": line_number,
                    "text": text,
                    "status": "retired" if section_retired else "unverified",
                }
            )
            continue

        if collecting and line and not line.startswith(" "):
            collecting = False

    return contracts


def inventory(spec_path: Path) -> dict[str, Any]:
    contracts = collect_contracts(spec_path)
    active = [contract for contract in contracts if contract["status"] != "retired"]
    domains: dict[str, dict[str, int]] = {}
    for contract in contracts:
        counts = domains.setdefault(contract["domain"], {"raw": 0, "active": 0, "retired": 0})
        counts["raw"] += 1
        if contract["status"] == "retired":
            counts["retired"] += 1
        else:
            counts["active"] += 1

    return {
        "spec": str(spec_path),
        "counts": {
            "raw": len(contracts),
            "active": len(active),
            "retired": len(contracts) - len(active),
        },
        "domains": domains,
        "contracts": contracts,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Inventory atomic SPEC acceptance contracts.")
    parser.add_argument("--spec", type=Path, default=ROOT / "SPEC.md")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    data = inventory(args.spec.resolve())
    rendered = json.dumps(data, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
