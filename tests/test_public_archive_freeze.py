#!/usr/bin/env python3
"""Public archive mutations must fail without private original evidence."""
from __future__ import annotations

import copy
import json
from pathlib import Path
import tempfile
import unittest

from check_public_archive_freeze import ROOT, MANIFEST, digest, validate_public_archive


class PublicArchiveFreezeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="public-archive-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.manifest = json.loads((ROOT / MANIFEST).read_text())
        names = {MANIFEST, *self.manifest["files"], *self.manifest["immutable_sha256"]}
        for name in names:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((ROOT / name).read_bytes())

    def write_json(self, name, document):
        (self.root / name).write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")

    def test_self_contained_public_archive_passes(self):
        self.assertEqual(validate_public_archive(self.root), [])

    def test_each_derived_file_is_required(self):
        for name in self.manifest["files"]:
            with self.subTest(name=name):
                path = self.root / name
                original = path.read_bytes()
                path.unlink()
                self.assertTrue(validate_public_archive(self.root))
                path.write_bytes(original)

    def test_missing_redaction_unknown_token_or_pointer_is_rejected(self):
        name = "docs/HOLDOUT_RESULTS.json"
        original = json.loads((self.root / name).read_text())
        for change in ("private-path", "unknown-token", "unknown-pointer", "missing-metadata"):
            with self.subTest(change=change):
                document = copy.deepcopy(original)
                if change == "private-path":
                    document["cases"][0]["cwd"] = "/home/private-user/private-run"
                if change == "unknown-token":
                    document["cases"][0]["cwd"] = "<UNAPPROVED_TOKEN>"
                if change == "unknown-pointer":
                    document["unexpected_redaction"] = "<REDACTED_PATH>"
                if change == "missing-metadata":
                    document.pop("_public_archive_derivative")
                self.write_json(name, document)
                self.assertTrue(validate_public_archive(self.root))
        self.write_json(name, original)

    def test_manifest_cannot_expand_allowed_pointers_or_tokens(self):
        for change in ("pointer", "token", "remove-protected"):
            with self.subTest(change=change):
                manifest = copy.deepcopy(self.manifest)
                allowed = manifest["files"]["docs/HOLDOUT_RESULTS.json"]["allowed_pointers"]
                if change == "pointer":
                    allowed["/cases/0/case"] = "<REDACTED_PATH>"
                if change == "token":
                    allowed["/cases/0/cwd"] = "<OTHER_TOKEN>"
                if change == "remove-protected":
                    manifest["immutable_sha256"].pop("tests/field_warning_contract.py")
                self.write_json(MANIFEST, manifest)
                self.assertTrue(validate_public_archive(self.root))
        self.write_json(MANIFEST, self.manifest)

    def test_non_path_historical_semantics_cannot_be_rehashed_away(self):
        name = "docs/HOLDOUT_RESULTS.json"
        original = json.loads((self.root / name).read_text())
        changes = ("score", "ranking", "case", "result", "reliability", "warning", "measurement")
        for change in changes:
            with self.subTest(change=change):
                document = copy.deepcopy(original)
                case = document["cases"][0]
                if change == "score": document["top5_hits"] += 1
                if change == "ranking": case["top5"].reverse()
                if change == "case": case["case"] = "other-case"
                if change == "result": case["diff_exit_code"] = 2
                if change == "reliability": case["reliability"]["left"]["capture"] = "incomplete"
                if change == "warning": case["runs"]["good"]["warnings"].append("new warning")
                if change == "measurement": case["runs"]["good"]["parsed_events"] += 1
                self.write_json(name, document)
                manifest = copy.deepcopy(self.manifest)
                manifest["files"][name]["sha256"] = digest((self.root / name).read_bytes())
                self.write_json(MANIFEST, manifest)
                errors = validate_public_archive(self.root)
                self.assertIn(f"historical semantics changed: {name}", errors)
        self.write_json(name, original)
        self.write_json(MANIFEST, self.manifest)

    def test_every_immutable_code_and_contract_is_protected(self):
        for name in self.manifest["immutable_sha256"]:
            with self.subTest(name=name):
                path = self.root / name
                original = path.read_bytes()
                path.write_bytes(original + b"\n")
                self.assertIn(f"historical code/contract changed: {name}", validate_public_archive(self.root))
                path.write_bytes(original)

    def test_markdown_paths_and_semantics_cannot_be_rehashed_away(self):
        for name, entry in self.manifest["files"].items():
            if not name.endswith(".md"):
                continue
            path = self.root / name
            original = path.read_text()
            token = entry["markdown_contract"]["spans"][0]["token"]
            for change in ("missing-token", "unknown-path", "shift-token", "nonpath-text"):
                with self.subTest(name=name, change=change):
                    if change == "missing-token": mutated = original.replace(token, "/home/private-user/private-report", 1)
                    if change == "unknown-path": mutated = original.replace(token, "/tmp/unapproved-report", 1)
                    if change == "shift-token": mutated = original.replace(token, " " + token, 1)
                    if change == "nonpath-text": mutated = original + "Altered historical measurement: 99\n"
                    path.write_text(mutated)
                    manifest = copy.deepcopy(self.manifest)
                    manifest["files"][name]["sha256"] = digest(path.read_bytes())
                    self.write_json(MANIFEST, manifest)
                    errors = validate_public_archive(self.root)
                    self.assertTrue(any(error.startswith(("Markdown redaction spans changed:",
                                                         "Markdown nonredacted content changed:")) for error in errors))
            path.write_text(original)
        self.write_json(MANIFEST, self.manifest)

    def test_markdown_redaction_cannot_be_removed_or_replaced(self):
        for name in self.manifest["files"]:
            if not name.endswith(".md"):
                continue
            with self.subTest(name=name):
                path = self.root / name
                original = path.read_text()
                for replacement in ("/home/private-user/private-report", "<UNAPPROVED_TOKEN>"):
                    path.write_text(original.replace("<REDACTED_TOOLCHAIN_PATH>", replacement)
                                    .replace("<REDACTED_LOCAL_PATH>", replacement))
                    self.assertTrue(validate_public_archive(self.root))
                path.write_text(original)


if __name__ == "__main__":
    unittest.main(verbosity=2)
