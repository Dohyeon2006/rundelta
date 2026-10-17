#!/usr/bin/env python3
"""Verify public redacted derivatives and immutable historical code/contracts.

This is a distinct public archive check. Original v1 report-byte verification
remains a private-tree operation; it is neither run nor reported as skipped here.
No private original or private archive path is required at runtime.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = "docs/REGRESSION_SET_PUBLIC.json"
MANIFEST_SHA256 = 'b50da2c3e378d3c7f690d1b3864d8903fe0561bd5ea250f4ec77a01ece787d61'
ALLOWED_REDACTIONS = {'docs/FIELD_CASES_RESULTS.json': {'/workdir': '<REDACTED_WORKDIR>'}, 'docs/HOLDOUT_RESULTS.json': {'/manifest/workdir': '<REDACTED_WORKDIR>', '/cases/0/cwd': '<REDACTED_WORKDIR>', '/cases/0/top5/2/subject': '<REDACTED_PATH> (openat)', '/cases/1/cwd': '<REDACTED_WORKDIR>', '/cases/1/top5/1/subject': '<REDACTED_PATH> (openat)', '/cases/2/cwd': '<REDACTED_WORKDIR>', '/cases/2/top5/1/subject': '<REDACTED_PATH> (openat)', '/cases/3/cwd': '<REDACTED_WORKDIR>', '/cases/3/top5/1/subject': '<REDACTED_PATH> (openat)', '/cases/3/top5/2/subject': '<REDACTED_PATH> (openat)', '/cases/3/top5/3/subject': '<REDACTED_PATH> (openat)', '/cases/3/top5/4/subject': '<REDACTED_PATH> (openat)', '/cases/4/cwd': '<REDACTED_WORKDIR>'}, 'docs/RANKING_REPAIR_RESULTS.json': {'/workdir': '<REDACTED_WORKDIR>', '/cases/0/top5/1/subject': '<REDACTED_PATH> (execve)', '/cases/0/top5/2/subject': '<REDACTED_PATH> (openat)', '/cases/0/top5/3/subject': '<REDACTED_PATH> (openat)', '/cases/0/top5/4/subject': '<REDACTED_PATH> (openat)', '/cases/1/top5/1/subject': '<REDACTED_PATH> (openat)', '/cases/1/top5/4/subject': '<REDACTED_PATH> (openat)', '/cases/2/top5/1/subject': '<REDACTED_PATH> (access)', '/cases/2/top5/4/subject': '<REDACTED_PATH> (openat)', '/cases/3/top5/1/subject': '<REDACTED_PATH> (openat)', '/cases/3/top5/3/subject': '<REDACTED_PATH> (execve)', '/cases/3/top5/4/subject': '<REDACTED_PATH> (execve)', '/cases/4/top5/1/subject': '<REDACTED_PATH> (execve)', '/cases/4/top5/2/subject': '<REDACTED_PATH> (openat)', '/cases/4/top5/3/subject': '<REDACTED_PATH> (openat)', '/cases/4/top5/4/subject': '<REDACTED_PATH> (execve)'}, 'docs/ACCEPTANCE-before-reliability.md': [{'source_line': 188, 'token': '<REDACTED_LOCAL_PATH>'}], 'docs/FIELD_CASES.md': [{'source_line': 100, 'token': '<REDACTED_TOOLCHAIN_PATH>'}, {'source_line': 158, 'token': '<REDACTED_TOOLCHAIN_PATH>'}], 'docs/RANKING_REPAIR.md': [{'source_line': 78, 'token': '<REDACTED_TOOLCHAIN_PATH>'}, {'source_line': 79, 'token': '<REDACTED_TOOLCHAIN_PATH>'}, {'source_line': 89, 'token': '<REDACTED_TOOLCHAIN_PATH>'}]}
DERIVATIVE_METADATA = {'kind': 'redacted_derivative', 'original_evidence': 'retained in private archive', 'measurements': 'historical; not rerun', 'scope': 'only explicitly listed local paths redacted'}

MARKDOWN_CONTRACTS = {'docs/ACCEPTANCE-before-reliability.md': {'spans': [{'line': 191, 'column': 11, 'token': '<REDACTED_LOCAL_PATH>', 'fragment': 'ARTIFACTS: <REDACTED_LOCAL_PATH>'}], 'nonredacted_sha256': 'c94eeda6773e9738580cbf2e86bd7cb9b5383243d623f776199442f869eb3b56'}, 'docs/FIELD_CASES.md': {'spans': [{'line': 103, 'column': 27, 'token': '<REDACTED_TOOLCHAIN_PATH>', 'fragment': '| 5 | Removed file access：`<REDACTED_TOOLCHAIN_PATH> (access)`；ENOENT → 未观察到 | 搜索噪声 |'}, {'line': 161, 'column': 26, 'token': '<REDACTED_TOOLCHAIN_PATH>', 'fragment': '| 3 | Executable changes：`<REDACTED_TOOLCHAIN_PATH> (execve)`；success → 未观察到 | 弱线索：未进入链接阶段 |'}], 'nonredacted_sha256': '0f3764104801544450a58fa21955b91fb67f887953ac54f404533d18664d1e66'}, 'docs/RANKING_REPAIR.md': {'spans': [{'line': 81, 'column': 35, 'token': '<REDACTED_TOOLCHAIN_PATH>', 'fragment': '| 4 | Execution attempt changes | `<REDACTED_TOOLCHAIN_PATH> (execve)` | ENOENT → 未观察到 |'}, {'line': 82, 'column': 35, 'token': '<REDACTED_TOOLCHAIN_PATH>', 'fragment': '| 5 | Execution attempt changes | `<REDACTED_TOOLCHAIN_PATH> (execve)` | ENOENT → 未观察到 |'}, {'line': 92, 'column': 28, 'token': '<REDACTED_TOOLCHAIN_PATH>', 'fragment': '| 5 | Executable changes | `<REDACTED_TOOLCHAIN_PATH> (execve)` | success → 未观察到 |'}], 'nonredacted_sha256': '4d8ef04d66c3013fde6665d537af6674b5f9ef0e7078cce81ea6471c38da26ee'}}


def digest(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def canonical(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def pointer_value(document: object, pointer: str) -> object:
    value = document
    for part in pointer.removeprefix("/").split("/"):
        value = value[int(part)] if isinstance(value, list) else value[part]
    return value


def validate_public_archive(root: Path) -> list[str]:
    """Return failures without loading any private originals or running tools."""
    errors = []
    try:
        encoded = (root / MANIFEST).read_bytes()
        manifest = json.loads(encoded)
        if digest(encoded) != MANIFEST_SHA256:
            errors.append("public archive manifest changed")
        if manifest.get("version") != 1 or set(manifest.get("files", {})) != set(ALLOWED_REDACTIONS):
            return errors + ["unexpected public archive file inventory"]
        for name, allowed in ALLOWED_REDACTIONS.items():
            entry = manifest["files"][name]
            content = (root / name).read_bytes()
            if digest(content) != entry["sha256"]:
                errors.append(f"public derivative bytes changed: {name}")
            text = content.decode("utf-8")
            if re.search(r"/home/[^/\s]+/", text):
                errors.append(f"unredacted private home path: {name}")
            if name.endswith(".json"):
                if entry.get("allowed_pointers") != allowed:
                    errors.append(f"unknown redaction pointer/token contract: {name}")
                document = json.loads(text)
                if document.pop("_public_archive_derivative", None) != DERIVATIVE_METADATA:
                    errors.append(f"missing/changed derivative metadata: {name}")
                for pointer, expected in allowed.items():
                    if pointer_value(document, pointer) != expected:
                        errors.append(f"incorrect redaction token: {name}{pointer}")
                # Canonical content fixes every non-path field and list order,
                # including case identities, scores, warnings and reliability.
                if digest(canonical(document)) != entry["semantic_sha256"]:
                    errors.append(f"historical semantics changed: {name}")
            else:
                contract = MARKDOWN_CONTRACTS[name]
                if entry.get("allowed_lines") != allowed or entry.get("markdown_contract") != contract:
                    errors.append(f"unknown Markdown redaction contract: {name}")
                lines = text.splitlines(keepends=True)
                actual_spans = []
                masked = []
                for index, line in enumerate(lines):
                    for match in re.finditer(r"<REDACTED_(?:LOCAL|TOOLCHAIN)_PATH>", line):
                        actual_spans.append(dict(line=index + 1, column=match.start(),
                                                 token=match[0], fragment=line.rstrip("\n")))
                    masked.append(re.sub(r"<REDACTED_(?:LOCAL|TOOLCHAIN)_PATH>", "<PATH_SLOT>", line))
                if actual_spans != contract["spans"]:
                    errors.append(f"Markdown redaction spans changed: {name}")
                if digest("".join(masked).encode()) != contract["nonredacted_sha256"]:
                    errors.append(f"Markdown nonredacted content changed: {name}")
        for name, expected in manifest["immutable_sha256"].items():
            if digest((root / name).read_bytes()) != expected:
                errors.append(f"historical code/contract changed: {name}")
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        errors.append(f"public archive cannot be verified: {error}")
    return errors


def main() -> None:
    errors = validate_public_archive(ROOT)
    if errors:
        raise SystemExit("\n".join(errors))
    print("PASS: public redacted archive and historical code/contracts verified; original v1 report evidence remains private")


if __name__ == "__main__":
    main()
