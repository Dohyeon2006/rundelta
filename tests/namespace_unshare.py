#!/usr/bin/env python3
"""Real Linux namespace-only unshare regression; requires user+network namespaces.

Run separately from the ordinary FS suite so its stronger host capability is
explicit. Failure to create the namespace is a failed environmental acceptance,
never a passing or skipped regression.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from acceptance_support import main, strace_environment

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("RUNDELTA_BIN", ROOT / "target/debug/rundelta")).resolve()
ENV = strace_environment(ROOT)
WORK = Path(tempfile.mkdtemp(prefix="namespace-unshare-", dir=ROOT / "target"))
PROBE = WORK / "probe"
STORE = WORK / "records"

# Run the recorder inside a user-owned namespace, then trace only this probe.
# The payload assertions demonstrate actual shared/private CWD behavior; path
# spellings and parser warnings remain separate observations.
PROBE_SOURCE = r"""#define _GNU_SOURCE
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

struct change { const char *destination; int notify; };
static int read_payload(char expected) {
    int fd = open("payload", O_RDONLY);
    char marker = 0;
    if (fd < 0) return 1;
    int failed = read(fd, &marker, 1) != 1 || marker != expected;
    close(fd);
    return failed;
}
static int change_namespace(void *argument) {
    struct change *change = argument;
    char marker = 'X';
    if (unshare(CLONE_NEWNET) == 0 && chdir(change->destination) == 0)
        marker = 'B';
    int sent = write(change->notify, &marker, 1) == 1;
    return marker == 'B' && sent ? 0 : 10;
}
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    int private_channel[2], shared_channel[2];
    if (pipe(private_channel) != 0) return 3;
    pid_t private_child = fork();
    if (private_child < 0) return 4;
    if (private_child == 0) {
        close(private_channel[1]);
        char marker;
        int failed = read(private_channel[0], &marker, 1) != 1 || read_payload('A');
        close(private_channel[0]);
        if (!failed) dprintf(STDOUT_FILENO, "private=A\n");
        _exit(failed ? 5 : 0);
    }
    close(private_channel[0]);
    int failed = pipe(shared_channel) != 0;
    pid_t shared_child = -1;
    char marker = 'X';
    unsigned char stack[65536];
    if (!failed) {
        struct change change = {argv[1], shared_channel[1]};
        shared_child = clone(change_namespace, stack + sizeof(stack),
                             CLONE_FS | SIGCHLD, &change);
        close(shared_channel[1]);
        if (shared_child > 0)
            failed = read(shared_channel[0], &marker, 1) != 1 || marker != 'B';
        else failed = 1;
        close(shared_channel[0]);
        failed |= read_payload('B');
    }
    if (!failed) dprintf(STDOUT_FILENO, "parent=B\n");
    failed |= write(private_channel[1], &marker, 1) != 1;
    close(private_channel[1]);
    int status;
    if (shared_child > 0)
        failed |= waitpid(shared_child, &status, 0) != shared_child ||
                  !WIFEXITED(status) || WEXITSTATUS(status) != 0;
    failed |= waitpid(private_child, &status, 0) != private_child ||
              !WIFEXITED(status) || WEXITSTATUS(status) != 0;
    return failed ? 6 : 0;
}
"""

class NamespaceUnshare(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        source = WORK / "probe.c"
        source.write_text(PROBE_SOURCE)
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-O2", str(source),
                        "-o", str(PROBE)], check=True)
        for name in ("A", "B"):
            directory = WORK / name
            directory.mkdir()
            (directory / "payload").write_text(name)

    def test_namespace_only_unshare_preserves_shared_fs_and_marks_connected_uncertain(self):
        unshare = shutil.which("unshare")
        self.assertIsNotNone(unshare, "util-linux unshare is required")
        recorded = subprocess.run(
            [unshare, "--user", "--map-root-user", str(BIN), "--storage", str(STORE),
             "record", "namespace-only", "--", str(PROBE), str(WORK / "B")],
            cwd=WORK / "A", env=ENV, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(recorded.returncode, 0, recorded.stdout + recorded.stderr)
        self.assertIn("parent=B", recorded.stdout)
        self.assertIn("private=A", recorded.stdout)
        def call(*args):
            return subprocess.run([str(BIN), "--storage", str(STORE), *args],
                                  env=ENV, capture_output=True, text=True, timeout=30)
        shown = call("show", "namespace-only", "--json")
        self.assertEqual(shown.returncode, 0, shown.stderr)
        run = json.loads(shown.stdout)
        metadata = run["metadata"]
        self.assertEqual(metadata["state"], "completed")
        self.assertEqual(metadata["completeness"], "partial")
        self.assertEqual(metadata["exit_code"], 0)
        self.assertEqual(metadata["collector_exit_code"], 0)
        self.assertEqual(len(metadata["warnings"]), 1, metadata["warnings"])
        self.assertIn("filesystem state was split", metadata["warnings"][0])
        transition = next(e for e in run["events"] if e["operation"] == "unshare")
        self.assertEqual(transition["outcome"], "success")
        self.assertTrue(transition["context_uncertain"])
        raw = (STORE / "runs" / metadata["id"] / "raw/trace.log").read_text().splitlines()
        self.assertIn("unshare(CLONE_NEWNET)", raw[transition["evidence"]["line"] - 1])
        payloads = [e for e in run["events"] if e["operation"] in ("open", "openat", "openat2")
                    and e["path"] in (str(WORK / "A" / "payload"), str(WORK / "B" / "payload"))]
        self.assertEqual(len(payloads), 2, payloads)
        parent = next(e for e in payloads if e["path"] == str(WORK / "B" / "payload"))
        private = next(e for e in payloads if e["path"] == str(WORK / "A" / "payload"))
        self.assertIsNone(parent["cwd_before"])
        self.assertTrue(parent["context_uncertain"])
        self.assertEqual(private["cwd_before"], str(WORK / "A"))
        self.assertFalse(private["context_uncertain"])
        for event in (transition, parent, private):
            self.assertEqual(event["schema_version"], 2)
            self.assertEqual(event["evidence"]["file"], "raw/trace.log")
            self.assertLessEqual(event["evidence"]["line"], event["evidence"]["end_line"])
        data, text = call("diff", "namespace-only", "namespace-only", "--json"), call("diff", "namespace-only", "namespace-only")
        self.assertEqual((data.returncode, text.returncode), (3, 3))
        report = json.loads(data.stdout)
        self.assertFalse(report["observed_differences"])
        self.assertEqual(report["reliability"]["left"], {"capture": "incomplete", "storage": "verified"})
        self.assertIn("RELIABILITY NOT ESTABLISHED", text.stdout)


if __name__ == "__main__":
    try:
        main()
    finally:
        print(f"ARTIFACTS: {WORK}")
