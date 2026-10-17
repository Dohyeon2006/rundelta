#!/usr/bin/env python3
"""Check only the independently frozen cwd-tree warning contract bytes."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "tests/fixtures/cwd_tree_warning_contract_v1.json"
FILES = ("tests/cwd_tree_warning_contract.py", "tests/test_cwd_tree_warning_contract.py")


def main():
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1 or set(manifest.get("sha256", {})) != set(FILES):
        raise SystemExit("invalid cwd-tree warning contract manifest")
    for name in FILES:
        actual = hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
        if actual != manifest["sha256"][name]:
            raise SystemExit(f"cwd-tree warning contract changed: {name}")
    print("cwd-tree warning contract v1: unchanged")


if __name__ == "__main__":
    main()
