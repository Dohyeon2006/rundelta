#!/usr/bin/env python3
"""In-memory safety counterexamples. No real personal paths or secrets."""
import base64
import importlib.util
import json
from pathlib import Path
import unittest
import urllib.parse

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("check_export", ROOT / "tools/check_export.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class ExportSafety(unittest.TestCase):
    def test_paths_and_encoded_variants(self):
        values = ["/" + root + "/example/private-file" for root in
                  ("home", "Users", "root", "tmp", "var/tmp", "private/tmp", "run/user")]
        values.append("C:" + chr(92) + "Users" + chr(92) + "example" + chr(92) + "file")
        for value in values:
            encodings = [value, json.dumps({"p": value}), json.dumps({"p": value}) + "\n",
                         value.encode().hex(), "".join("\\x%02x" % b for b in value.encode()),
                         "".join("\\u%04x" % ord(c) for c in value),
                         "".join("\\%03o" % b for b in value.encode()),
                         urllib.parse.quote(value, safe=""),
                         base64.b64encode(value.encode()).decode(),
                         base64.urlsafe_b64encode(value.encode()).decode().rstrip("=")]
            encodings += [json.dumps(json.dumps(s)) for s in encodings]
            b64 = base64.b64encode(value.encode()).decode()
            encodings.append("\n".join(b64[i:i + 8] for i in range(0, len(b64), 8)))
            encodings.append(base64.b64encode(base64.b64encode(value.encode())).decode())
            encodings.append(base64.b64encode(value.encode().hex().encode()).decode())
            for i, encoded in enumerate(encodings):
                with self.subTest(variant=i):
                    self.assertTrue(CHECK.privacy("README.md", encoded.encode()))

    def test_material_and_secret_encodings(self):
        values = ["docs/" + "external-" + "alpha/record.json",
                  "." + "idea/local.xml", "docs/" + "performance/raw.json",
                  "gh" + "p_" + "a" * 30,
                  "-----BEGIN " + "PRIVATE KEY-----"]
        for value in values:
            for encoded in (value, value.encode().hex(), base64.b64encode(value.encode()).decode()):
                self.assertTrue(CHECK.privacy("README.md", encoded.encode()))

    def test_constants_have_exact_file_and_value_scope(self):
        for name, constants in CHECK.CONSTANTS.items():
            for value in constants:
                self.assertFalse(CHECK.privacy(name, json.dumps(value).encode()))
                self.assertTrue(CHECK.privacy("README.md", json.dumps(value).encode()))
                self.assertTrue(CHECK.privacy(name, json.dumps(value + "/private").encode()))

    def test_nonstandard_private_prefix(self):
        value = "/" + "private-work-root" + "/data"
        for encoded in (value, base64.b64encode(value.encode()).decode(), value.encode().hex()):
            self.assertTrue(CHECK.privacy("README.md", encoded.encode(), [value]))

    def test_inventory_rejects_extra_missing_and_wrong_license(self):
        files = CHECK.worktree_files(ROOT)
        self.assertFalse(CHECK.inspect_files(files, CHECK.WORKTREE))
        for name in ("target/program", "docs/" + "external-" + "alpha/data.json",
                     "." + "idea/config", "unexpected.txt"):
            self.assertTrue(CHECK.inspect_files(files | {name: b"synthetic"}, CHECK.WORKTREE))
        self.assertTrue(CHECK.inspect_files({k: v for k, v in files.items() if k != "LICENSE"}, CHECK.WORKTREE))
        self.assertTrue(CHECK.inspect_files(files | {"LICENSE": b"incorrect"}, CHECK.WORKTREE))


if __name__ == "__main__":
    unittest.main(verbosity=2)
