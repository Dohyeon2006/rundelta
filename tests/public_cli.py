#!/usr/bin/env python3
"""Independent Linux workflow; all records are generated in a disposable directory."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class PublicCLI(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = str(Path(os.environ["RUNDELTA_BIN"]).resolve())
        cls.env = dict(os.environ)
        cls.temp = tempfile.TemporaryDirectory(prefix="rundelta-public-cli-")
        cls.addClassCleanup(cls.temp.cleanup)
        cls.work = Path(cls.temp.name)
        cls.store = cls.work / "records"
        source = cls.work / "file_access.c"
        source.write_text("""#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    int fd = open(argv[1], O_RDONLY);
    if (fd < 0) return errno == ENOENT ? 1 : 2;
    return close(fd) == 0 ? 0 : 2;
}
""")
        cls.target = cls.work / "file-access"
        compiled = subprocess.run(["cc", str(source), "-o", str(cls.target)],
                                  capture_output=True, timeout=60)
        if compiled.returncode:
            raise AssertionError("test helper compilation failed")
        cls.data = cls.work / "input with spaces.txt"
        cls.data.write_text("public synthetic input\n")
        for name in ("good", "repeat"):
            if cls.call("record", name, "--", str(cls.target), str(cls.data)).returncode != 0:
                raise AssertionError("normal real capture failed")
        cls.data.unlink()
        if cls.call("record", "missing", "--", str(cls.target), str(cls.data)).returncode != 1:
            raise AssertionError("missing-file target exit was not preserved")
        cls.data.write_text("public synthetic input\n")

    @classmethod
    def call(cls, *args, store=None, env=None, stdout=subprocess.PIPE):
        return subprocess.run([cls.binary, "--storage", str(store or cls.store), *args],
                              cwd=cls.work, env=env or cls.env, stdout=stdout,
                              stderr=subprocess.PIPE, timeout=30)

    def test_help_and_version(self):
        for option in ("--help", "--version"):
            result = self.call(option)
            self.assertEqual(result.returncode, 0)
            self.assertTrue(result.stdout)

    def test_complete_verified_real_records(self):
        for name, code in (("good", 0), ("repeat", 0), ("missing", 1)):
            result = self.call("show", name, "--json")
            self.assertEqual(result.returncode, 0)
            record = json.loads(result.stdout)
            self.assertEqual(record["metadata"]["completeness"], "complete_for_supported_events")
            self.assertEqual(record["metadata"]["exit_code"], code)
            self.assertEqual(record["metadata"]["warnings"], [])
            self.assertEqual(record["storage_integrity"]["status"], "verified")
            matches = [e for e in record["events"] if e.get("path") == str(self.data)]
            self.assertTrue(matches)
            self.assertTrue(all(e["outcome"] == ("success" if code == 0 else "ENOENT")
                                for e in matches))
            self.assertTrue(all(e["evidence"]["line"] > 0 for e in matches))

    def test_diff_zero_and_one_text_json(self):
        for right, code in (("repeat", 0), ("missing", 1)):
            for suffix in ((), ("--json",)):
                result = self.call("diff", "good", right, *suffix)
                self.assertEqual(result.returncode, code)
                if suffix:
                    report = json.loads(result.stdout)
                    self.assertEqual(report["exit_code"], code)
                    self.assertEqual(report["observed_differences"], bool(code))
                elif code:
                    self.assertIn(b"ENOENT", result.stdout)

    def test_list_and_show_text_json(self):
        for command in (("list",), ("show", "good")):
            for suffix in ((), ("--json",)):
                result = self.call(*command, *suffix)
                self.assertEqual(result.returncode, 0)
                self.assertTrue(result.stdout)
                if suffix:
                    self.assertTrue(json.loads(result.stdout))

    def damaged_copy(self, label):
        store = self.work / label
        shutil.copytree(self.store, store)
        record = next(p for p in (store / "runs").iterdir()
                      if json.loads((p / "metadata.json").read_text())["name"] == "good")
        return store, record

    def test_diff_three_unfinalized(self):
        store, record = self.damaged_copy("unfinalized-copy")
        (record / "finalized.json").unlink()
        for suffix in ((), ("--json",)):
            result = self.call("diff", "good", "repeat", *suffix, store=store)
            self.assertEqual(result.returncode, 3)
            if suffix:
                self.assertEqual(json.loads(result.stdout)["exit_code"], 3)

    def test_diff_two_corrupt(self):
        store, record = self.damaged_copy("corrupt-copy")
        (record / "events.jsonl").write_bytes(b"")
        for suffix in ((), ("--json",)):
            result = self.call("diff", "good", "repeat", *suffix, store=store)
            self.assertEqual(result.returncode, 2)
            if suffix:
                report = json.loads(result.stdout)
                self.assertEqual(report["exit_code"], 2)
                self.assertIsNone(report["observed_differences"])

    def test_missing_strace_never_runs_target(self):
        marker = self.work / "must-not-exist"
        env = dict(self.env, RUNDELTA_STRACE=str(self.work / "nonexistent-tracer"))
        result = self.call("record", "no-tracer", "--", "/bin/sh", "-c",
                           ': > "$1"', "sh", str(marker), env=env)
        self.assertEqual(result.returncode, 2)
        self.assertIn(b"strace unavailable", result.stderr)
        self.assertFalse(marker.exists())

    def test_broken_pipe_no_panic(self):
        for suffix in ((), ("--json",)):
            reader, writer = os.pipe()
            os.close(reader)
            try:
                result = self.call("show", "good", *suffix, stdout=writer)
            finally:
                os.close(writer)
            self.assertEqual(result.returncode, 0)
            self.assertNotIn(b"panicked", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
