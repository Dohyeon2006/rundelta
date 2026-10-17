#!/usr/bin/env python3
"""Capture real mount-topology transitions in a user-owned mount namespace.

Capability detection belongs to tests/full_gate.py. This acceptance test never
turns an unavailable host capability into an apparent passing test.
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


from acceptance_support import strace_environment

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("RUNDELTA_BIN", ROOT / "target/debug/rundelta")).resolve()
STRACE = strace_environment(ROOT)["RUNDELTA_STRACE"]
WORK = Path(tempfile.mkdtemp(prefix="mount-namespace-", dir=ROOT / "target"))
STORE = WORK / "records"


class MountNamespace(unittest.TestCase):
    def test_real_mount_and_unmount_are_preserved_and_fail_closed(self):
        unshare = shutil.which("unshare")
        self.assertIsNotNone(unshare, "full gate must preflight util-linux unshare")
        mountpoint = WORK / "mountpoint"
        mountpoint.mkdir()
        command = [
            unshare,
            "--user",
            "--map-root-user",
            "--mount",
            "/bin/sh",
            "-c",
            'mount -t tmpfs none "$1" && umount "$1"',
            "mount-probe",
            str(mountpoint),
        ]
        environment = os.environ | {"RUNDELTA_STRACE": STRACE}
        recorded = subprocess.run(
            [
                str(BIN),
                "--storage",
                str(STORE),
                "record",
                "real-user-mountns",
                "--",
                *command,
            ],
            cwd=WORK,
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(recorded.returncode, 0, recorded.stderr)

        shown = subprocess.run(
            [
                str(BIN),
                "--storage",
                str(STORE),
                "show",
                "real-user-mountns",
                "--json",
            ],
            cwd=WORK,
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(shown.returncode, 0, shown.stderr)
        run = json.loads(shown.stdout)
        self.assertEqual(run["metadata"]["state"], "completed")
        self.assertEqual(run["metadata"]["completeness"], "partial")
        operations = {
            event["operation"]
            for event in run["events"]
            if event["outcome"] == "success"
        }
        self.assertIn("mount", operations)
        self.assertIn("umount2", operations)
        raw = (
            STORE / "runs" / run["metadata"]["id"] / "raw" / "trace.log"
        ).read_text()
        self.assertIn("mount(", raw)
        self.assertIn("umount2(", raw)

        report = subprocess.run(
            [
                str(BIN),
                "--storage",
                str(STORE),
                "diff",
                "real-user-mountns",
                "real-user-mountns",
                "--json",
            ],
            cwd=WORK,
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(report.returncode, 3, report.stdout + report.stderr)
        comparison = json.loads(report.stdout)
        self.assertFalse(comparison["observed_differences"])
        self.assertEqual(
            comparison["reliability"]["left"],
            {"capture": "incomplete", "storage": "verified"},
        )


if __name__ == "__main__":
    from acceptance_support import main
    main()
