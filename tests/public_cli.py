#!/usr/bin/env python3
"""Independent Linux checks; every record and helper is generated temporarily.

--synthetic needs Python 3 and the Linux process APIs used by RunDelta, but no
installed strace or C compiler. Its explicit test backend never executes a
target. --real additionally needs cc and a compatible strace and exercises real
capture. Neither mode imports private scripts or consumes saved run evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


SYNTHETIC_TRACE = (
    '11 execve("/synthetic/tool", ["tool"], 0x0) = 0\n'
    '11 openat(AT_FDCWD, "/synthetic/input", O_RDONLY) = 3</synthetic/input>\n'
    '11 +++ exited with 0 +++\n'
).encode()

# This backend deliberately supplies test evidence, not a real observation.
# Its bytes and all optional markers are supplied from the owning temporary
# directory. It does not spawn descendants, so recorder timeout cleanup cannot
# leave a detached test target behind.
SYNTHETIC_BACKEND = '''import os
from pathlib import Path
import sys
import time

marker = os.environ.get("PUBLIC_TEST_BACKEND_MARKER")
if marker:
    Path(marker).write_text("backend started", encoding="ascii")
if "--version" in sys.argv[1:]:
    print("RunDelta synthetic test backend; does not execute targets")
else:
    destination = Path(sys.argv[sys.argv.index("-o") + 1])
    destination.write_bytes(Path(os.environ["PUBLIC_TEST_TRACE"]).read_bytes())
    if os.environ.get("PUBLIC_TEST_WAIT") == "1":
        time.sleep(30)
'''


class SyntheticEnvironment(unittest.TestCase):
    def setUp(self):
        self.binary = str(Path(os.environ["RUNDELTA_BIN"]).resolve())
        temporary = tempfile.TemporaryDirectory(prefix="rundelta-synthetic-cli-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name)
        self.store = self.work / "records"
        self.trace = self.work / "synthetic-input.txt"
        self.trace.write_bytes(SYNTHETIC_TRACE)
        self.backend = self.work / "synthetic-backend.py"
        self.backend.write_text("#!" + sys.executable + "\n" + SYNTHETIC_BACKEND,
                                encoding="utf-8")
        self.backend.chmod(0o700)
        self.marker = self.work / "backend-started"
        self.env = dict(os.environ, RUNDELTA_STRACE=str(self.backend),
                        PUBLIC_TEST_TRACE=str(self.trace),
                        PUBLIC_TEST_BACKEND_MARKER=str(self.marker),
                        PUBLIC_TEST_WAIT="0", PYTHONDONTWRITEBYTECODE="1")

    def call(self, *args, nofile=None, env=None, launcher=()):
        # This unittest runner is single-threaded; only the child changes its
        # own descriptor limit, immediately before exec of the tested binary.
        def limit_fds():
            resource.setrlimit(resource.RLIMIT_NOFILE, (nofile, nofile))

        return subprocess.run(
            [*launcher, self.binary, "--storage", str(self.store), *args],
            cwd=self.work, env=self.env if env is None else env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10,
            preexec_fn=limit_fds if nofile is not None else None,
        )

    def record(self, name, *options, **kwargs):
        return self.call("record", name, *options, "--", "synthetic-target", **kwargs)

    def show(self, name):
        shown = self.call("show", name, "--json")
        self.assertEqual(shown.returncode, 0, shown.stderr)
        return json.loads(shown.stdout)

    def assert_incomplete(self, name, completeness="incomplete"):
        run = self.show(name)
        self.assertEqual(run["metadata"]["schema_version"], 3)
        self.assertEqual(run["metadata"]["completeness"], completeness)
        # Integrity proves finalization, not a complete capture.
        self.assertEqual(run["storage_integrity"]["status"], "verified")
        for suffix in ((), ("--json",)):
            compared = self.call("diff", name, name, *suffix)
            self.assertEqual(compared.returncode, 3, compared.stderr)
            if suffix:
                report = json.loads(compared.stdout)
                self.assertEqual(report["schema_version"], 4)
                self.assertEqual(report["exit_code"], 3)
        return run


class SyntheticCLI(SyntheticEnvironment):
    def test_default_limits_keep_complete_synthetic_evidence(self):
        help_output = self.call("record", "--help")
        self.assertEqual(help_output.returncode, 0, help_output.stderr)
        for option in (b"--max-duration-ms", b"--max-trace-bytes", b"--terminate-grace-ms"):
            self.assertIn(option, help_output.stdout)
        self.assertIn(b"default: 2000", help_output.stdout)
        recorded = self.record("complete")
        self.assertEqual(recorded.returncode, 0, recorded.stderr)
        run = self.show("complete")
        self.assertEqual(run["metadata"]["schema_version"], 3)
        self.assertEqual(run["metadata"]["completeness"], "complete_for_supported_events")
        self.assertEqual(run["storage_integrity"]["status"], "verified")
        self.assertTrue(run["events"])
        self.assertTrue(all(event["schema_version"] == 2 for event in run["events"]))
        self.assertTrue(all(event["evidence"]["line"] > 0 for event in run["events"]))
        compared = self.call("diff", "complete", "complete", "--json")
        self.assertEqual(compared.returncode, 0, compared.stderr)
        self.assertEqual(json.loads(compared.stdout)["exit_code"], 0)

    def test_trace_limit_is_strict_and_does_not_publish_prefix_events(self):
        for limit, expected in ((len(SYNTHETIC_TRACE), 0), (len(SYNTHETIC_TRACE) - 1, 125)):
            with self.subTest(limit=limit):
                name = "bytes-" + str(limit)
                recorded = self.record(name, "--max-trace-bytes", str(limit),
                                       "--terminate-grace-ms", "10")
                self.assertEqual(recorded.returncode, expected, recorded.stderr)
                if expected:
                    run = self.assert_incomplete(name)
                    self.assertEqual(run["events"], [])
                    self.assertEqual(run["metadata"]["state"], "capture_failed")
                    self.assertTrue(any("trace byte limit exceeded" in warning
                                        for warning in run["metadata"]["warnings"]))
                else:
                    self.assertEqual(self.show(name)["metadata"]["completeness"],
                                     "complete_for_supported_events")

    def test_duration_limit_finalizes_incomplete_without_running_a_target(self):
        recorded = self.record("duration", "--max-duration-ms", "100",
                               "--terminate-grace-ms", "10",
                               env=dict(self.env, PUBLIC_TEST_WAIT="1"))
        self.assertEqual(recorded.returncode, 125, recorded.stderr)
        run = self.assert_incomplete("duration")
        self.assertEqual(run["metadata"]["state"], "capture_failed")
        self.assertTrue(any("capture duration limit exceeded" in warning
                            for warning in run["metadata"]["warnings"]))
        # A subsequent reservation also exercises release of the storage lock.
        self.assertEqual(self.record("after-duration").returncode, 0)

    def test_late_invalid_utf8_fails_closed(self):
        self.trace.write_bytes(SYNTHETIC_TRACE + b"\xff\n")
        recorded = self.record("late-utf8")
        self.assertEqual(recorded.returncode, 125, recorded.stderr)
        run = self.assert_incomplete("late-utf8")
        self.assertEqual(run["events"], [])
        self.assertEqual(run["metadata"]["parsed_events"], 0)
        self.assertEqual(run["metadata"]["raw_lines"], 0)

    def test_relative_operand_is_not_promoted_from_cwd(self):
        self.trace.write_bytes(SYNTHETIC_TRACE.replace(
            b'"/synthetic/input", O_RDONLY) = 3</synthetic/input>',
            b'"relative-input", O_RDONLY) = -1 ENOENT (No such file)'))
        recorded = self.record("relative")
        self.assertEqual(recorded.returncode, 0, recorded.stderr)
        run = self.assert_incomplete("relative", completeness="partial")
        event = next(event for event in run["events"] if event["operation"] == "openat")
        self.assertEqual(event["path"], "relative:relative-input [cwd unresolved]")
        self.assertTrue(event["context_uncertain"])
        self.assertEqual(event["evidence"]["line"], 2)
        self.assertTrue(run["metadata"]["warnings"])

    def test_ignored_sigchld_rejects_before_backend_spawn(self):
        launcher = (sys.executable, "-c",
                    "import os,signal,sys; "
                    "signal.signal(signal.SIGCHLD, signal.SIG_IGN); "
                    "os.execv(sys.argv[1], sys.argv[1:])")
        recorded = self.record("ignored-sigchld", launcher=launcher)
        self.assertEqual(recorded.returncode, 2, recorded.stderr)
        self.assertIn(b"SIGCHLD disposition", recorded.stderr)
        self.assertFalse(self.marker.exists(), "even the version probe must not start")
        self.assertFalse(any((self.store / "runs").iterdir()))

    def populate_records(self):
        recorded = self.record("label-000")
        self.assertEqual(recorded.returncode, 0, recorded.stderr)
        original = next((self.store / "runs").iterdir())
        metadata = json.loads((original / "metadata.json").read_bytes())
        marker = json.loads((original / "finalized.json").read_bytes())
        self.assertEqual(marker["schema_version"], 1)
        labels = ["label-000"]
        for index in range(1, 70):
            run_id = f"a{index:08x}-101"
            label = f"label-{index:03d}"
            directory = self.store / "runs" / run_id
            shutil.copytree(original, directory)
            data = json.dumps(dict(metadata, id=run_id, name=label)).encode()
            (directory / "metadata.json").write_bytes(data)
            finalized = dict(marker, run_id=run_id, metadata={
                "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
            })
            (directory / "finalized.json").write_text(json.dumps(finalized), encoding="utf-8")
            labels.append(label)
        return labels

    def test_low_fd_scan_preserves_every_label_and_rejects_duplicates(self):
        labels = self.populate_records()
        self.marker.unlink()
        before = {directory.name for directory in (self.store / "runs").iterdir()}
        listing = self.call("list", "--json", nofile=64)
        self.assertEqual(listing.returncode, 0, listing.stderr)
        rows = json.loads(listing.stdout)
        self.assertEqual({row["name"] for row in rows}, set(labels))
        self.assertEqual(len(rows), len(labels))
        self.assertTrue(all(row["storage_integrity"]["status"] == "verified" for row in rows))
        for label in reversed(labels):
            duplicate = self.record(label, nofile=64)
            self.assertEqual(duplicate.returncode, 2, duplicate.stderr)
            self.assertIn(b"record name already exists", duplicate.stderr)
        self.assertEqual({directory.name for directory in (self.store / "runs").iterdir()}, before)
        self.assertFalse(self.marker.exists(), "duplicate labels must not probe a backend")

    def test_fd_scan_error_rejects_reservation_without_backend_side_effects(self):
        self.assertEqual(self.record("existing").returncode, 0)
        self.marker.unlink()
        before = {directory.name for directory in (self.store / "runs").iterdir()}
        for limit in (7, 8):
            with self.subTest(nofile=limit):
                recorded = self.record("must-not-exist", nofile=limit)
                self.assertEqual(recorded.returncode, 2, recorded.stderr)
                self.assertIn(b"Too many open files", recorded.stderr)
                self.assertNotIn(b"corrupt", recorded.stderr)
                self.assertFalse(self.marker.exists())
                self.assertEqual({directory.name for directory in (self.store / "runs").iterdir()},
                                 before)


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
        self.assertIn(b"failed to start supervised strace version probe", result.stderr)
        self.assertIn(b"RUNDELTA_STRACE", result.stderr)
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


class FinalizationCLI(SyntheticEnvironment):
    """Inject signals only after capture facts freeze; never launch a target."""

    def setUp(self):
        super().setUp()
        self.barrier = self.work / "finalization-barrier.so"
        fixture = Path(__file__).resolve().parent / "fixtures/finalization_barrier.c"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-shared",
                        str(fixture), "-ldl", "-o", str(self.barrier)],
                       check=True, capture_output=True, timeout=60)

    def finalization_case(self, mode, number, fail):
        case = self.work / f"case-{mode}-{number}-{fail}"
        case.mkdir()
        self.store = case / "records"
        ready, release = case / "ready", case / "release"
        os.mkfifo(release, 0o600)
        gate = os.open(release, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
        env = dict(self.env, LD_PRELOAD=str(self.barrier),
                   RF_FINALIZE_READY=str(ready), RF_FINALIZE_RELEASE=str(release))
        env.pop("RF_FINALIZE_FAIL", None)
        if fail:
            env["RF_FINALIZE_FAIL"] = "1"
        # Python normally installs an INT handler. Explicitly set the inherited
        # dispositions and mask before exec; no native launcher is required.
        launcher = (
            "import os,signal,sys; "
            "controls=(signal.SIGHUP,signal.SIGINT,signal.SIGTERM); "
            "[signal.signal(s,signal.SIG_DFL) for s in controls]; "
            "signal.pthread_sigmask(signal.SIG_UNBLOCK,controls); "
            "mode=sys.argv[1]; number=int(sys.argv[2]); "
            "signal.signal(number,signal.SIG_IGN) if mode=='ignored' else None; "
            "signal.pthread_sigmask(signal.SIG_BLOCK,[number]) if mode=='blocked' else None; "
            "os.execv(sys.argv[3],sys.argv[3:])"
        )
        process, pidfd = None, None
        try:
            process = subprocess.Popen(
                [sys.executable, "-c", launcher, mode, str(number or 0), self.binary,
                 "--storage", str(self.store), "record", "late", "--", "synthetic-target"],
                cwd=case, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            pidfd = os.pidfd_open(process.pid)
            deadline = time.monotonic() + 10
            while not ready.exists():
                self.assertIsNone(process.poll(), "recorder exited before publication")
                self.assertLess(time.monotonic(), deadline, "publication barrier timeout")
                time.sleep(0.005)
            if number is not None:
                signal.pidfd_send_signal(pidfd, number)
                status = dict(line.split(":", 1) for line in
                              Path(f"/proc/{process.pid}/status").read_text().splitlines())
                bit = 1 << (number - 1)
                self.assertTrue(int(status["SigBlk"], 16) & bit)
                self.assertTrue((int(status["SigPnd"], 16) | int(status["ShdPnd"], 16)) & bit)
            os.write(gate, b"x")
            stdout, stderr = process.communicate(timeout=10)
            expected = -number if number is not None and mode == "default" else (2 if fail else 0)
            self.assertEqual(process.returncode, expected, (stdout, stderr))
        finally:
            if process is not None and process.poll() is None:
                # Only this owned recorder is signalled; no numeric-PID fallback.
                # The finite backend neither executes a target nor forks children.
                if pidfd is not None:
                    try:
                        signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                else:
                    os.write(gate, b"x")
                process.communicate(timeout=5)
            if pidfd is not None:
                os.close(pidfd)
            os.close(gate)
        run = self.show("late")
        self.assertEqual(run["storage_integrity"]["status"], "unfinalized" if fail else "verified")
        self.assertEqual(run["metadata"]["state"], "completed")
        self.assertEqual((run["metadata"]["exit_code"], run["metadata"]["signal"]), (0, None))
        self.assertEqual(run["metadata"]["warnings"], [])
        record = self.store / "runs" / run["metadata"]["id"]
        self.assertEqual((record / "finalized.json").exists(), not fail)
        self.assertEqual(list(record.glob(".rundelta-tmp-*")), [])
        deleted = self.call("delete", "late")
        self.assertEqual(deleted.returncode, 0, "recorder retained its storage lock")

    def test_late_signals_preserve_dispositions_and_finalization_facts(self):
        for fail in (False, True):
            with self.subTest(signal="none", fail=fail):
                self.finalization_case("default", None, fail)
            for number in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
                for mode in ("default", "ignored", "blocked"):
                    with self.subTest(signal=number, mode=mode, fail=fail):
                        self.finalization_case(mode, number, fail)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--synthetic", action="store_true", help="no strace or cc required")
    mode.add_argument("--real", action="store_true", help="real strace capture workflow")
    selected = parser.parse_args()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        SyntheticCLI if selected.synthetic else PublicCLI)
    if selected.real:
        suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(FinalizationCLI))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    passed = result.wasSuccessful() and result.testsRun > 0 and not (
        result.skipped or result.expectedFailures)
    raise SystemExit(0 if passed else 1)
