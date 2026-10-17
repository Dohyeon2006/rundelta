#!/usr/bin/env python3
"""Characterize the current Unix exec and recorder-cancellation boundaries.

The direct and recorded targets are the same C probe. The launcher makes the
caller's signal dispositions, mask, and closed descriptors explicit immediately
before exec. SIGPIPE and standard-fd expectations remain known DEC-04/RF-720
limitations; the HUP/INT/TERM expectations are RF-430 contracts.
"""

from __future__ import annotations

from contextlib import ExitStack
import ctypes
import errno
import json
import os
from pathlib import Path
import platform
import shutil
import select
import signal
import struct
import subprocess
import sys
import tempfile
import time
import unittest

from acceptance_support import main, require_owned_group_context, strace_environment
from capture_owner_support import BrokerReceiptTests, FaultBroker, compile_probe


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("RUNDELTA_BIN", ROOT / "target/debug/rundelta")).resolve()


# Host-thread controls run before exec; syscall faults use an external owned-child tracer.
EXTRA_THREAD_GUARD_PROBE = (ROOT / "tests/probes/capture_host_hooks.c").read_text(encoding="ascii")


def executable(path: str | Path) -> bool:
    candidate = Path(path)
    return candidate.is_file() and os.access(candidate, os.X_OK)


def watch_for_atomic_move(directory: Path) -> int:
    libc = ctypes.CDLL(None, use_errno=True)
    libc.inotify_init1.argtypes = [ctypes.c_int]
    libc.inotify_init1.restype = ctypes.c_int
    libc.inotify_add_watch.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
    libc.inotify_add_watch.restype = ctypes.c_int
    descriptor = libc.inotify_init1(os.O_CLOEXEC | os.O_NONBLOCK)
    if descriptor < 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))
    watch = libc.inotify_add_watch(descriptor, os.fsencode(directory), 0x00000080)
    if watch < 0:
        error = ctypes.get_errno()
        os.close(descriptor)
        raise OSError(error, os.strerror(error))
    return descriptor


def wait_for_atomic_move(descriptor: int, target_name: str, deadline: float) -> None:
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([descriptor], [], [], remaining)[0]:
            raise AssertionError("readiness target was not atomically renamed")
        data = os.read(descriptor, 4096)
        offset = 0
        while offset + 16 <= len(data):
            _watch, mask, _cookie, length = struct.unpack_from("iIII", data, offset)
            name = data[offset + 16:offset + 16 + length].split(b"\0", 1)[0].decode("ascii")
            if mask & 0x00000080 and name == target_name:
                return
            offset += 16 + length


def parse_ready_pid(contents: str) -> int:
    if not contents.endswith("\n") or contents.count("\n") != 1:
        raise ValueError("incomplete readiness PID marker")
    digits = contents[:-1]
    if not digits.isascii() or not digits.isdecimal() or int(digits) <= 0:
        raise ValueError("invalid readiness PID")
    return int(digits)


def parse_owner_child(contents: str, *, allow_zero_child: bool = False) -> tuple[int, int]:
    if not contents.endswith("\n") or contents.count("\n") != 1:
        raise ValueError("incomplete owner/child marker")
    fields = contents[:-1].split(" ")
    if len(fields) != 2:
        raise ValueError("owner/child marker must have exactly two fields")
    parsed = {}
    for field in fields:
        parts = field.split("=")
        if len(parts) != 2 or parts[0] not in {"owner", "child"} or parts[0] in parsed:
            raise ValueError("invalid owner/child fields")
        if not parts[1].isascii() or not parts[1].isdecimal():
            raise ValueError("invalid owner/child PID")
        parsed[parts[0]] = int(parts[1])
    if set(parsed) != {"owner", "child"} or parsed["owner"] <= 0:
        raise ValueError("owner/child fields are incomplete")
    child = parsed["child"]
    if child == 0 and allow_zero_child:
        return parsed["owner"], child
    if child <= 0 or child == parsed["owner"]:
        raise ValueError("owner and child must be distinct positive PIDs")
    return parsed["owner"], child


class IsolatedRunner:
    """Bound a private group while its unreaped leader protects the numeric PGID.

    The runner requires default SIGCHLD and no competing reaper or native
    SA_NOCLDWAIT mutation throughout the child lifetime. Output uses files so a
    descendant cannot hold a pipe open or make communicate() reap the leader
    before group cleanup. Every group operation precedes the final wait().
    """

    def __init__(self, cwd: Path, environment: dict[str, str]):
        self.cwd = cwd
        self.environment = environment

    @staticmethod
    def _signal_group(process_group: int, signal_number: int) -> None:
        try:
            os.killpg(process_group, signal_number)
        except ProcessLookupError:
            pass

    @staticmethod
    def _live_group_members(process_group: int) -> bool:
        """Inspect the protected group, excluding its retained zombie leader.

        killpg(0) also reports the zombie used to retain ownership. Only live
        members establish an orphan here; unexpected inspection errors cannot
        be treated as a clean group. A vanished process needs no observation.
        """
        for entry in Path("/proc").iterdir():
            if not entry.name.isdecimal():
                continue
            try:
                fields = (entry / "stat").read_bytes().rsplit(b") ", 1)[1].split()
            except (FileNotFoundError, ProcessLookupError):
                continue
            if int(fields[2]) == process_group and fields[0] not in {b"Z", b"X"}:
                return True
        return False

    def run(
        self,
        command: list[str],
        timeout: float = 20.0,
        *,
        environment: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[bytes]:
        return self._execute(command, timeout, environment=environment)

    def signal_when_ready(
        self,
        command: list[str],
        ready: Path,
        signal_numbers: tuple[int, ...],
        *,
        environment: dict[str, str] | None = None,
        release: Path | None = None,
        observe_ready_pid: bool = False,
        timeout: float = 20.0,
    ) -> subprocess.CompletedProcess[bytes]:
        return self._execute(
            command, timeout, environment=environment, ready=ready,
            signal_numbers=signal_numbers, release=release,
            observe_ready_pid=observe_ready_pid,
        )

    def _execute(
        self,
        command: list[str],
        timeout: float,
        *,
        environment: dict[str, str] | None = None,
        ready: Path | None = None,
        signal_numbers: tuple[int, ...] = (),
        release: Path | None = None,
        observe_ready_pid: bool = False,
    ) -> subprocess.CompletedProcess[bytes]:
        child_environment = self.environment.copy()
        if environment is not None:
            child_environment.update(environment)
        require_owned_group_context()
        with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors, ExitStack() as handles:
            process = subprocess.Popen(
                command, cwd=self.cwd, env=child_environment,
                stdout=output, stderr=errors, start_new_session=True,
            )
            child_owned = True
            pidfd = None
            observed_pidfd = None
            failure = None
            orphaned = False
            operation_error = None
            cleanup_errors = []

            def exited() -> bool:
                nonlocal child_owned
                try:
                    return os.waitid(
                        os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT
                    ) is not None
                except ChildProcessError:
                    # A failed ownership premise is never permission to signal
                    # a numeric group ID that may already have been reused.
                    child_owned = False
                    raise

            try:
                pidfd = os.pidfd_open(process.pid)
                handles.callback(os.close, pidfd)
                deadline = time.monotonic() + timeout
                if ready is not None:
                    observed_pid: int | None = None
                    while True:
                        if ready.exists():
                            if not observe_ready_pid:
                                break
                            try:
                                observed_pid = parse_ready_pid(ready.read_text(encoding="ascii"))
                            except ValueError:
                                observed_pid = None
                            if observed_pid is not None:
                                break
                        if exited():
                            failure = "process exited before readiness"
                            break
                        if time.monotonic() >= deadline:
                            failure = "readiness timeout"
                            break
                        time.sleep(0.01)
                    if failure is None:
                        if observe_ready_pid:
                            # The controlled readiness protocol must keep this
                            # original process live until observation. No later
                            # numeric lookup may substitute for this handle.
                            assert observed_pid is not None
                            observed_pidfd = os.pidfd_open(observed_pid)
                            handles.callback(os.close, observed_pidfd)
                        if release is not None:
                            release.write_text("go\n", encoding="ascii")
                        for signal_number in signal_numbers:
                            try:
                                signal.pidfd_send_signal(pidfd, signal_number)
                            except ProcessLookupError:
                                break
                if failure is None:
                    while not exited():
                        if time.monotonic() >= deadline:
                            failure = "post-signal timeout" if ready is not None else "timeout"
                            break
                        time.sleep(0.01)
                if failure is None:
                    orphaned = self._live_group_members(process.pid)
                    if orphaned:
                        failure = "orphaned process group"
                    if observed_pidfd is not None:
                        try:
                            signal.pidfd_send_signal(observed_pidfd, 0)
                        except ProcessLookupError:
                            pass
                        else:
                            failure = failure or "observed supervised child remained alive"
            except BaseException as error:
                operation_error = error
            finally:
                try:
                    if child_owned:
                        # Revalidate even on setup/readiness exceptions, before
                        # any numeric signal. Retain the leader through cleanup.
                        exited()
                        self._signal_group(process.pid, signal.SIGKILL)
                except BaseException as error:
                    cleanup_errors.append(error)
                # One refused delivery must not skip another stable identity.
                # Lost wait authority still forbids numeric group operations,
                # while these already acquired handles cannot follow PID reuse.
                for descriptor in (observed_pidfd, pidfd):
                    if descriptor is not None:
                        try:
                            signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        except BaseException as error:
                            cleanup_errors.append(error)
                if child_owned:
                    try:
                        process.wait(timeout=5)
                    except BaseException as error:
                        cleanup_errors.append(error)
            output.seek(0)
            errors.seek(0)
            stdout, stderr = output.read(), errors.read()
        if operation_error is None and failure is not None:
            operation_error = AssertionError(
                f"{failure} (orphaned={orphaned}): {command!r}\n"
                f"stdout={stdout.decode(errors='replace')!r}\n"
                f"stderr={stderr.decode(errors='replace')!r}"
            )
        if cleanup_errors:
            diagnostic = RuntimeError("capture runner cleanup failed: " + "; ".join(
                f"{type(error).__name__}: {error}" for error in cleanup_errors
            ))
            if operation_error is not None:
                # Keep ECHILD/cancellation/setup failure as the primary fact;
                # every failed cleanup attempt remains visible as its cause.
                raise operation_error from diagnostic
            raise diagnostic from cleanup_errors[0]
        if operation_error is not None:
            raise operation_error
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def parse_probe_report(path: Path) -> dict[str, str | int]:
    fields: dict[str, str | int] = {}
    for item in path.read_text(encoding="ascii").split():
        key, value = item.split("=", 1)
        fields[key] = (
            int(value)
            if key.startswith("fd") or key.startswith("blocked_")
            else value
        )
    expected = {
        "hup",
        "int",
        "term",
        "pipe",
        "blocked_hup",
        "blocked_int",
        "blocked_term",
        "fd0",
        "fd1",
        "fd2",
    }
    if fields.keys() != expected:
        raise AssertionError(f"probe report fields changed: {fields!r}")
    return fields


def default_probe_report() -> dict[str, str | int]:
    return {
        "hup": "D",
        "int": "D",
        "term": "D",
        "pipe": "D",
        "blocked_hup": 0,
        "blocked_int": 0,
        "blocked_term": 0,
        "fd0": 1,
        "fd1": 1,
        "fd2": 1,
    }


class ReadinessFormatTests(unittest.TestCase):
    def test_single_pid_requires_complete_positive_line(self):
        self.assertEqual(parse_ready_pid("123\n"), 123)
        for invalid in ("", "123", "123\n456\n", "0\n", "-1\n", "123 456\n"):
            with self.subTest(marker=invalid), self.assertRaises(ValueError):
                parse_ready_pid(invalid)

    def test_owner_child_requires_exact_distinct_pid_fields(self):
        self.assertEqual(parse_owner_child("owner=123 child=456\n"), (123, 456))
        for invalid in ("owner=123\n", "owner=123 child=123\n",
                        "owner=123 child=456 extra=789\n", "owner=123 child=\n",
                        "owner=123 child=456 child=789\n"):
            with self.subTest(marker=invalid), self.assertRaises(ValueError):
                parse_owner_child(invalid)
        self.assertEqual(parse_owner_child("owner=123 child=0\n", allow_zero_child=True), (123, 0))


class RuntimeBoundary(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if platform.system() != "Linux":
            raise unittest.SkipTest("Linux is required")
        if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
            raise unittest.SkipTest("pidfd_open and pidfd_send_signal are required")
        if shutil.which("cc") is None:
            raise unittest.SkipTest("a C compiler is required")
        if not executable(BIN):
            raise RuntimeError(f"RunDelta binary is not executable: {BIN}")

        target = ROOT / "target"
        target.mkdir(exist_ok=True)
        cls.temporary = tempfile.TemporaryDirectory(
            prefix="runtime-boundary-", dir=target
        )
        cls.work = Path(cls.temporary.name)
        cls.probe = cls.work / "runtime-boundary-probe"
        builds = 0
        def compiled(argv, **unused):
            nonlocal builds
            builds += 1
            return compile_probe(argv, cwd=ROOT, evidence=cls.work / f"compile-{builds:02d}")
        try:
            compiled(
                [
                    "cc",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-O2",
                    str(ROOT / "tests/fixtures/runtime_boundary_probe.c"),
                    "-o",
                    str(cls.probe),
                ],
                check=True,
                cwd=ROOT,
            )
            extra_thread_source = cls.work / "extra-thread-guard-probe.c"
            extra_thread_source.write_text(EXTRA_THREAD_GUARD_PROBE, encoding="ascii")
            cls.extra_thread_probe = cls.work / "extra-thread-guard-probe.so"
            compiled(
                ["cc", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-shared",
                 "-pthread", str(extra_thread_source), "-ldl", "-o", str(cls.extra_thread_probe)],
                check=True, cwd=ROOT,
            )
            cls.pthread_eagain = cls.work / "pthread-eagain.so"
            compiled(
                [
                    "cc",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-O2",
                    "-fPIC",
                    "-shared",
                    str(ROOT / "tests/fixtures/pthread_eagain.c"),
                    "-o",
                    str(cls.pthread_eagain),
                ],
                check=True,
                cwd=ROOT,
            )
            cls.request_fault = cls.work / "request-fault.so"
            compiled(["cc", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-shared",
                            str(ROOT / "tests/probes/capture_request_fault.c"), "-ldl", "-o", str(cls.request_fault)],
                           check=True, cwd=ROOT)
            cls.chld_target = cls.work / "chld-target"
            compiled(["cc", "-Wall", "-Wextra", "-Werror", "-O2",
                            str(ROOT / "tests/probes/capture_chld_target.c"), "-o", str(cls.chld_target)],
                           check=True, cwd=ROOT)
            cls.caught_control = cls.work / "caught-control.so"
            compiled(["cc", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-shared",
                            str(ROOT / "tests/probes/capture_caught_control.c"), "-o", str(cls.caught_control)],
                           check=True, cwd=ROOT)
            environment = strace_environment(ROOT)
            backend = environment["RUNDELTA_STRACE"]
            cls.finalization_backend = cls.work / "finalization-backend.py"
            trace = ('11 execve("/synthetic/tool", ["tool"], 0x0) = 0\n'
                     '11 +++ exited with 0 +++\n')
            cls.finalization_backend.write_text(
                "#!" + sys.executable + "\nimport sys\nfrom pathlib import Path\n"
                "if '--version' in sys.argv:\n print('synthetic backend; no target')\n"
                "else:\n Path(sys.argv[sys.argv.index('-o') + 1]).write_text("
                + repr(trace) + ")\n", encoding="utf-8",
            )
            cls.finalization_backend.chmod(0o700)
            cls.version_backend = cls.work / "version-backend.py"
            shutil.copyfile(
                ROOT / "tests/fixtures/version_backend.py",
                cls.version_backend,
            )
            cls.version_backend.chmod(0o700)
            environment["RF440_REAL_STRACE"] = backend
            environment["PYTHONDONTWRITEBYTECODE"] = "1"
            cls.runner = IsolatedRunner(cls.work, environment)
            cls.faults = FaultBroker(cls.work / "owner-syscall-faults")
        except BaseException:
            cls.temporary.cleanup()
            raise

    @classmethod
    def tearDownClass(cls) -> None:
        cls.temporary.cleanup()

    def finalization_case(self, mode: str, number: int | None, fail: bool,
                          *, environment_extra=None, expected_status=None) -> None:
        with tempfile.TemporaryDirectory(prefix="finalization-", dir=self.work) as directory:
            work = Path(directory)
            ready, release, storage = work / "ready", work / "release", work / "records"
            os.mkfifo(release, 0o600)
            gate = os.open(release, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
            environment = self.runner.environment | {
                "RUNDELTA_STRACE": str(self.finalization_backend),
                "CAPTURE_FAULT_READY": str(ready), "CAPTURE_FAULT_RELEASE": str(release),
            }
            environment.update(environment_extra or {})
            if fail:
                environment["CAPTURE_FAULT_FAIL"] = "1"
            folder, argv, environment = self.faults.prepare(
                [str(self.probe), "launch", mode, "0", str(BIN),
                 "--storage", str(storage), "record", "late", "--", "synthetic-target"],
                storage, "finalize", environment, work)
            process = None
            pidfd = None
            start = time.monotonic_ns()
            try:
                process = subprocess.Popen(argv, cwd=work, env=environment,
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                deadline = time.monotonic() + 10
                while not ready.exists():
                    self.assertIsNone(process.poll(), "fault controller exited before publication")
                    self.assertLess(time.monotonic(), deadline, "publication barrier timeout")
                    time.sleep(0.005)
                owner, _child = parse_owner_child(ready.read_text(encoding="ascii"), allow_zero_child=True)
                pidfd = os.pidfd_open(owner)
                if number is not None:
                    signal.pidfd_send_signal(pidfd, number)
                    status = dict(line.split(":", 1) for line in
                                  Path(f"/proc/{owner}/status").read_text().splitlines())
                    bit = 1 << (number - 1)
                    self.assertTrue(int(status["SigBlk"], 16) & bit)
                    self.assertTrue((int(status["SigPnd"], 16) |
                                     int(status["ShdPnd"], 16)) & bit)
                os.write(gate, b"x")
                output, errors = process.communicate(timeout=10)
                outer = subprocess.CompletedProcess(argv, process.returncode, output, errors)
                result = self.faults.result(folder, outer, time.monotonic_ns() - start)
                expected = -number if number is not None and mode == "neutral" else (2 if fail else 0)
                if expected_status is not None:
                    expected = expected_status
                self.assertEqual(result.returncode, expected, (result.stdout, result.stderr))
            finally:
                if process is not None and process.poll() is None:
                    # Release first. The broker only owns this forked subject;
                    # EXITKILL handles a failed harness without a reused PID.
                    os.write(gate, b"x")
                    process.communicate(timeout=5)
                if pidfd is not None:
                    os.close(pidfd)
                os.close(gate)
            shown = self.runner.run([str(BIN), "--storage", str(storage), "show", "late", "--json"])
            self.assertEqual(shown.returncode, 0, shown.stderr)
            run = json.loads(shown.stdout)
            self.assertEqual(run["storage_integrity"]["status"], "unfinalized" if fail else "verified")
            self.assertEqual(run["metadata"]["state"], "completed")
            self.assertEqual((run["metadata"]["exit_code"], run["metadata"]["signal"]), (0, None))
            self.assertEqual(run["metadata"]["warnings"], [])
            record = storage / "runs" / run["metadata"]["id"]
            self.assertEqual((record / "finalized.json").exists(), not fail)
            self.assertEqual(list(record.glob(".rundelta-tmp-*")), [])
            deleted = self.runner.run([str(BIN), "--storage", str(storage), "delete", "late"])
            self.assertEqual(deleted.returncode, 0, "recorder retained its storage lock")

    def test_finalization_late_signals_keep_original_dispositions(self) -> None:
        for fail in (False, True):
            with self.subTest(signal="none", fail=fail):
                self.finalization_case("neutral", None, fail)
            for suffix, number in (("hup", signal.SIGHUP), ("int", signal.SIGINT),
                                   ("term", signal.SIGTERM)):
                for mode in ("neutral", "ignore-" + suffix, "block-" + suffix):
                    with self.subTest(signal=number, mode=mode, fail=fail):
                        self.finalization_case(mode, number, fail)

    def test_old_caught_callback_becomes_default_after_owner_exec(self):
        audit = self.work / "old-caught-control-audit"
        self.finalization_case("neutral", signal.SIGINT, False,
                               environment_extra={"LD_PRELOAD": str(self.caught_control),
                                                  "RD_CAUGHT_RECORDER": str(BIN), "RD_CAUGHT_AUDIT": str(audit)})
        self.assertEqual(audit.read_bytes(), b"old-image-initialized\n",
                         "old host callback remained callable in the owner image")

    def restoration_case(self, contract, mode):
        # The clean owner deliberately does not load host interposers. The
        # external owned-child tracer changes the actual syscall return after
        # taking an immutable copy of the already committed record.
        case = f"external-restore-{contract}-{mode}"
        work = self.work / case
        work.mkdir(mode=0o700)
        storage, target_report = work / "records", work / "target.txt"
        option = ["--evidence-contract", "trace-v1"] if contract == "trace-v1" else []
        recorded = self.faults.run(
            [str(self.probe), "launch", "neutral", "0", str(BIN),
             "--storage", str(storage), "record", case, *option, "--",
             str(self.probe), "target", str(target_report)], storage, mode,
            environment=self.runner.environment, cwd=self.work, runner=self.runner)
        self.assertEqual(recorded.returncode, 0 if mode == "none" else 2, recorded.stderr)
        self.assertEqual(parse_probe_report(target_report), default_probe_report())
        shown = self.runner.run([str(BIN), "--storage", str(storage), "show", case, "--json"])
        self.assertEqual(shown.returncode, 0, shown.stderr)
        run = json.loads(shown.stdout)
        metadata = run["metadata"]
        record = storage / "runs" / metadata["id"]
        snapshot = self.faults.committed(recorded) / metadata["id"]
        original = {p.relative_to(snapshot): p.read_bytes() for p in snapshot.rglob("*") if p.is_file()}
        final = {p.relative_to(record): p.read_bytes() for p in record.rglob("*") if p.is_file()}
        self.assertEqual(final, original, "restoration rewrote committed evidence")
        self.assertEqual(metadata, json.loads(original[Path("metadata.json")]))
        self.assertEqual((metadata["state"], metadata["completeness"], metadata["exit_code"],
                          metadata["signal"], metadata["collector_exit_code"], metadata["collector_signal"]),
                         ("completed", "complete_for_supported_events", 0, None, 0, None))
        self.assertEqual(metadata["warnings"], [])
        self.assertEqual(run["storage_integrity"]["status"], "verified")
        self.assertEqual(metadata["schema_version"], 4 if contract == "trace-v1" else 3)
        marker = json.loads(original[Path("finalized.json")])
        self.assertEqual(marker["schema_version"], 2 if contract == "trace-v1" else 1)
        self.assertEqual(run["events"], [json.loads(line) for line in original[Path("events.jsonl")].splitlines()])
        if contract == "trace-v1":
            self.assertEqual(run["raw_integrity"]["status"], "verified")
            provenance = metadata["target_provenance"]
            for field, kind in [("root_exec", "exec"), ("target_exit", "exit")]:
                reference = provenance[field]
                event = run["events"][reference["index"]]
                self.assertEqual((event["kind"], event["evidence"], event["pid"], event["task_generation"]),
                                 (kind, reference["evidence"], provenance["root"]["pid"], provenance["root"]["generation"]))
        else:
            self.assertNotIn("raw_integrity", run)
            self.assertNotIn(Path("raw-integrity.json"), original)
        expected = {"none": 0, "one-action": 1, "one-mask": 1, "restore-actions": 6, "restore-mask": 2}[mode]
        self.assertEqual(self.faults.denials(recorded), expected)
        events = (recorded.evidence / "events.txt").read_text()
        self.assertNotRegex(events, r"event=signal-exit .*return=-38")
        if mode != "none":
            self.assertIn(f"record {metadata['id']} committed but failed to restore recorder signal state".encode(), recorded.stderr)
            self.assertIn(b"restore actions:" if "action" in mode else b"restore mask:", recorded.stderr)
        final_bits = {key: int(value, 16) for key, value in __import__('re').findall(
            r"event=signals stage=before-exit (SigBlk|SigIgn):\s*([0-9a-f]+)", events)}
        controls = (1 << 0) | (1 << 1) | (1 << 14)
        self.assertEqual(final_bits["SigBlk"] & controls, controls if mode.startswith("restore-") else 0)
        pids = {int(value) for value in __import__('re').findall(r"event=owned-child-wait child=(\d+)", events)}
        pids |= {int(value) for value in __import__('re').findall(r"event=exec execution=\d+ pid=(\d+)", events)}
        self.assertGreaterEqual(len(pids), 3, "probe/formal child wait observations missing")
        for pid in pids:
            self.assertFalse(Path(f"/proc/{pid}").exists(), "owned child remains alive after restoration")
        for suffix in ([], ["--json"]):
            compared = self.runner.run([str(BIN), "--storage", str(storage), "diff", case, case, *suffix])
            self.assertEqual(compared.returncode, 0, compared.stderr)
            if suffix:
                result = json.loads(compared.stdout)
                self.assertEqual((result["schema_version"], result["exit_code"], result["changes"], result["errors"]),
                                 (5 if contract == "trace-v1" else 4, 0, [], []))
            else:
                self.assertIn(b"Exit code: 0", compared.stdout)
        self.assert_storage_lock_reacquired(storage, case)
        deleted = self.runner.run([str(BIN), "--storage", str(storage), "delete", case])
        self.assertEqual(deleted.returncode, 0, deleted.stderr)

    def test_private_owner_admission_and_actual_low_descriptor_transport(self):
        valid = ["none", "offset", "low-0", "low-1", "low-2"]
        invalid = ["magic", "version", "contract", "flags", "reserved", "zero-command", "counts",
                   "total", "length", "truncated", "unsealed", "closed-fd", "stdio-fd", "negative-fd",
                   "overflow-fd", "native-fd", "missing-fd", "extra-argument", "linked"]
        for mode in valid + invalid:
            with self.subTest(mode=mode):
                storage = self.work / ("request-" + mode + "-storage")
                target = self.work / ("request-" + mode + "-target")
                audit = self.work / ("request-" + mode + "-audit")
                result = self.runner.run([str(BIN), "--storage", str(storage), "record", mode, "--",
                                          "/usr/bin/touch", str(target)],
                                         environment={"LD_PRELOAD": str(self.request_fault),
                                                      "RD_REQUEST_RECORDER": str(BIN), "RD_REQUEST_MODE": mode,
                                                      "RD_REQUEST_LINKED": str(self.work / ("request-" + mode + "-linked")),
                                                      "RD_REQUEST_AUDIT": str(audit)})
                self.assertEqual(result.returncode, 0 if mode in valid else 2, result.stderr)
                rows = [dict(field.split("=") for field in line.split()) for line in audit.read_text().splitlines()]
                self.assertTrue(any(row["event"] == "owner-exec" for row in rows), "private admission was not executed")
                if mode in valid:
                    self.assertTrue(target.exists())
                    self.assertEqual(self.show(storage, mode)["storage_integrity"]["status"], "verified")
                else:
                    self.assertFalse(target.exists(), "malformed private request ran target")
                    self.assertFalse(storage.exists(), "malformed private request opened storage")
                if mode == "linked":
                    flags = next(row for row in rows if row["event"] == "linked-descriptor-flags")
                    self.assertEqual((int(flags["result"]), int(flags["errno"])), (0, 0),
                                     "linked descriptor did not cross exec open without CLOEXEC")
                    self.assertIn(b"capture request must be anonymous regular storage", result.stderr,
                                  "linked case did not reach the fresh-owner inode admission")
                if mode.startswith("low-"):
                    low = int(mode[-1])
                    created = next(row for row in rows if row["event"] == "memfd-created")
                    copied = next(row for row in rows if row["event"] == "request-duplicated")
                    closed = next(row for row in rows if row["event"] == "low-slot-after-dup")
                    self.assertEqual(int(created["result"]), low, "low-FD branch was never reached")
                    self.assertEqual(int(copied["fd"]), low)
                    self.assertGreaterEqual(int(copied["result"]), 3)
                    self.assertEqual((int(closed["result"]), int(closed["errno"])), (-1, errno.EBADF))
        direct = self.runner.run([str(BIN), "__rundelta_capture_owner_v1", "3"])
        self.assertEqual(direct.returncode, 2)
        self.assertIn(b"unrecognized subcommand", direct.stderr)

    def test_ignored_no_cld_wait_has_direct_exec_semantics_at_target_boundary(self):
        storage, report = self.work / "no-cld-wait-storage", self.work / "no-cld-wait-target"
        audit = self.work / "no-cld-wait-audit"
        result = self.faults.run([str(BIN), "--storage", str(storage), "record", "no-cld-wait", "--",
                                 str(self.chld_target), str(report)], storage,
                                environment=self.runner.environment | {
                                    "LD_PRELOAD": str(self.request_fault), "RD_REQUEST_RECORDER": str(BIN),
                                    "RD_REQUEST_MODE": "chld-no-cld-wait", "RD_REQUEST_AUDIT": str(audit)},
                                cwd=self.work, runner=self.runner)
        self.assertEqual(result.returncode, 0, result.stderr)
        before = [dict(field.split("=") for field in line.split()) for line in audit.read_text().splitlines()]
        self.assertTrue(any(row["event"] == "chld-before-owner-exec" and row["result"] == "1" for row in before))
        # Linux exec keeps SIG_IGN but clears action flags. Demonstrate this
        # independently rather than requiring an impossible flag inheritance.
        direct_report = self.work / "no-cld-wait-direct"
        direct = self.runner.run([str(self.chld_target), "launch", str(direct_report)])
        self.assertEqual(direct.returncode, 0, direct.stderr)
        self.assertEqual(direct.stdout, b"before_exec ignored=1 no_cld_wait=1\n")
        self.assertEqual(report.read_bytes(), direct_report.read_bytes())
        self.assertEqual(report.read_text(), "ignored=1 no_cld_wait=0\n")
        events = (result.evidence / "events.txt").read_text()
        final = __import__('re').findall(r"event=signals stage=before-exit SigIgn:\s*([0-9a-f]+)", events)
        self.assertTrue(int(final[-1], 16) & (1 << (signal.SIGCHLD - 1)))
        run = self.show(storage, "no-cld-wait")
        self.assertEqual((run["metadata"]["state"], run["metadata"]["exit_code"], run["storage_integrity"]["status"]),
                         ("completed", 0, "verified"))

    def test_owner_sigchld_scope_reports_initialization_and_committed_restore_failures(self):
        for inherited in ("default", "ignored"):
            for mode in ("none", "init-chld", "one-chld", "restore-chld"):
                with self.subTest(inherited=inherited, mode=mode):
                    case = "owner-chld-" + inherited + "-" + mode
                    storage, target = self.work / (case + "-storage"), self.work / (case + "-target")
                    command = [str(BIN), "--storage", str(storage), "record", case, "--", "/usr/bin/touch", str(target)]
                    if inherited == "ignored":
                        command = [sys.executable, "-B", "-c", "import os,signal,sys; signal.signal(signal.SIGCHLD,signal.SIG_IGN); os.execv(sys.argv[1],sys.argv[1:])", *command]
                    result = self.faults.run(command, storage, mode,
                                             environment=self.runner.environment,
                                             cwd=self.work, runner=self.runner)
                    self.assertEqual(result.returncode, 0 if mode == "none" else 2, result.stderr)
                    self.assertEqual(self.faults.denials(result), {"none": 0, "init-chld": 1, "one-chld": 1, "restore-chld": 2}[mode])
                    if mode == "init-chld":
                        self.assertIn(b"establish exclusive capture-owner SIGCHLD waitability", result.stderr)
                        self.assertFalse(target.exists())
                        self.assertFalse(storage.exists(), "CHLD setup failure acquired storage")
                    else:
                        self.assertTrue(target.exists())
                        run = self.show(storage, case)
                        record = storage / "runs" / run["metadata"]["id"]
                        snapshot = self.faults.committed(result) / record.name
                        self.assertEqual({p.relative_to(snapshot): p.read_bytes() for p in snapshot.rglob("*") if p.is_file()},
                                         {p.relative_to(record): p.read_bytes() for p in record.rglob("*") if p.is_file()})
                        self.assertEqual((run["metadata"]["state"], run["metadata"]["exit_code"], run["storage_integrity"]["status"]),
                                         ("completed", 0, "verified"))
                        if mode != "none":
                            self.assertIn(f"record {record.name} committed but failed to restore recorder signal state".encode(), result.stderr)
                            self.assertIn(b"SIGCHLD", result.stderr)
                        events = (result.evidence / "events.txt").read_text()
                        ignored = __import__('re').findall(r"event=signals stage=before-exit SigIgn:\s*([0-9a-f]+)", events)
                        self.assertTrue(ignored)
                        final_ignore = int(ignored[-1], 16) & (1 << (signal.SIGCHLD - 1))
                        self.assertEqual(bool(final_ignore), inherited == "ignored" and mode != "restore-chld")
                        self.assert_storage_lock_reacquired(storage, case)

    def test_committed_record_survives_parent_signal_restoration_failure(self):
        for contract in ("legacy", "trace-v1"):
            for mode in ("none", "one-action", "one-mask"):
                with self.subTest(contract=contract, mode=mode):
                    self.restoration_case(contract, mode)

    def test_committed_record_survives_permanent_parent_signal_restoration_failure(self):
        for contract in ("legacy", "trace-v1"):
            for mode in ("restore-actions", "restore-mask"):
                with self.subTest(contract=contract, mode=mode):
                    self.restoration_case(contract, mode)

    def launch(
        self,
        mode: str,
        close_mask: int,
        command: list[str],
    ) -> subprocess.CompletedProcess[bytes]:
        return self.runner.run(
            [
                str(self.probe),
                "launch",
                mode,
                str(close_mask),
                *command,
            ]
        )

    def direct(self, case: str, mode: str, close_mask: int = 0) -> tuple[subprocess.CompletedProcess[bytes], dict[str, str | int]]:
        report = self.work / f"{case}-direct.txt"
        completed = self.launch(
            mode,
            close_mask,
            [str(self.probe), "target", str(report)],
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode(errors="replace"))
        return completed, parse_probe_report(report)

    def record(
        self,
        case: str,
        mode: str,
        close_mask: int = 0,
    ) -> tuple[subprocess.CompletedProcess[bytes], dict[str, str | int], dict[str, object]]:
        report = self.work / f"{case}-record.txt"
        storage = self.work / f"{case}-storage"
        recorded = self.launch(
            mode,
            close_mask,
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                case,
                "--",
                str(self.probe),
                "target",
                str(report),
            ],
        )
        observed = parse_probe_report(report)

        shown = self.runner.run(
            [str(BIN), "--storage", str(storage), "show", case, "--json"]
        )
        self.assertEqual(shown.returncode, 0, shown.stderr.decode(errors="replace"))
        run = json.loads(shown.stdout)
        metadata = run["metadata"]
        self.assertEqual(metadata["schema_version"], 3)
        self.assertEqual(metadata["state"], "completed")
        self.assertEqual(metadata["completeness"], "complete_for_supported_events")
        self.assertEqual((metadata["exit_code"], metadata["signal"]), (0, None))
        self.assertEqual(
            (metadata["collector_exit_code"], metadata["collector_signal"]),
            (0, None),
        )
        self.assertEqual(metadata["target_exit_raw"], "+++ exited with 0 +++")
        self.assertEqual(metadata["warnings"], [])
        self.assertEqual(run["storage_integrity"]["status"], "verified")
        self.assertTrue(all(event["schema_version"] == 2 for event in run["events"]))

        compared = self.runner.run(
            [str(BIN), "--storage", str(storage), "diff", case, case, "--json"]
        )
        compared_text = self.runner.run(
            [str(BIN), "--storage", str(storage), "diff", case, case]
        )
        self.assertEqual((compared.returncode, compared_text.returncode), (0, 0))
        comparison = json.loads(compared.stdout)
        complete = {"capture": "complete", "storage": "verified"}
        self.assertEqual(comparison["schema_version"], 4)
        self.assertEqual(comparison["exit_code"], 0)
        self.assertIs(comparison["observed_differences"], False)
        self.assertEqual(comparison["reliability"], {"left": complete, "right": complete})
        self.assertEqual(comparison["changes"], [])
        self.assertEqual(comparison["errors"], [])
        self.assertIn(b"Exit code: 0", compared_text.stdout)
        return recorded, observed, run

    def show(self, storage: Path, name: str) -> dict[str, object]:
        shown = self.runner.run(
            [str(BIN), "--storage", str(storage), "show", name, "--json"]
        )
        self.assertEqual(shown.returncode, 0, shown.stderr.decode(errors="replace"))
        return json.loads(shown.stdout)

    def assert_incomplete_self_diff(self, storage: Path, name: str) -> None:
        compared = self.runner.run(
            [str(BIN), "--storage", str(storage), "diff", name, name, "--json"]
        )
        self.assertEqual(compared.returncode, 3, compared.stderr.decode(errors="replace"))
        report = json.loads(compared.stdout)
        self.assertEqual(report["schema_version"], 4)
        self.assertEqual(report["exit_code"], 3)
        incomplete = {"capture": "incomplete", "storage": "verified"}
        self.assertEqual(report["reliability"], {"left": incomplete, "right": incomplete})

    def assert_storage_lock_reacquired(self, storage: Path, case: str) -> None:
        report = self.work / f"{case}-follow-up.txt"
        follow_up = self.runner.run(
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                f"{case}-follow-up",
                "--",
                str(self.probe),
                "target",
                str(report),
            ]
        )
        self.assertEqual(
            follow_up.returncode,
            0,
            follow_up.stderr.decode(errors="replace"),
        )
        self.assertEqual(parse_probe_report(report), default_probe_report())

    def assert_no_record_started(self, storage: Path) -> None:
        runs = storage / "runs"
        self.assertTrue(runs.is_dir(), f"storage root was not initialized: {storage}")
        self.assertEqual(list(runs.iterdir()), [], "probe failure created a record")

    def test_ignored_control_signals_are_preserved_at_record_boundary(self) -> None:
        # SignalScope temporarily makes these dispositions observable in the
        # recorder, while child pre_exec restores the caller contract exactly.
        for mode, selected in [
            ("ignore-hup", "hup"),
            ("ignore-int", "int"),
            ("ignore-term", "term"),
        ]:
            with self.subTest(mode=mode):
                case = f"control-{selected}"
                _, direct = self.direct(case, mode)
                recorded, observed, _ = self.record(case, mode)
                expected = default_probe_report()
                expected[selected] = "I"
                self.assertEqual(direct, expected)
                self.assertEqual(observed, expected)
                self.assertEqual(recorded.returncode, 0)

    def test_ignored_sigchld_is_normalized_in_owner_and_retained_for_target(self):
        storage = self.work / "ignored-sigchld-storage"
        target = self.work / "ignored-sigchld-target"
        # A real strace backend has its own CHLD supervision normalization; use
        # a shell target that directly reports its inherited ignored disposition.
        launcher = "import os,signal,sys; signal.signal(signal.SIGCHLD, signal.SIG_IGN); os.execv(sys.argv[1],sys.argv[1:])"
        command = [sys.executable, "-B", "-c", launcher, str(BIN), "--storage", str(storage),
                   "record", "ignored-sigchld", "--", sys.executable, "-B", "-c",
                   "import pathlib,signal,sys; pathlib.Path(sys.argv[1]).write_text(str(signal.getsignal(signal.SIGCHLD)))", str(target)]
        recorded = self.runner.run(command)
        self.assertEqual(recorded.returncode, 0, recorded.stderr)
        self.assertEqual(target.read_text(), str(signal.SIG_IGN))
        self.assertEqual(self.show(storage, "ignored-sigchld")["metadata"]["state"], "completed")

    def test_blocked_control_signal_mask_is_preserved_at_record_boundary(self) -> None:
        _, direct = self.direct("blocked-hup", "block-hup")
        recorded, observed, _ = self.record("blocked-hup", "block-hup")
        expected = default_probe_report()
        expected["blocked_hup"] = 1
        self.assertEqual(direct, expected)
        self.assertEqual(observed, expected)
        self.assertEqual(recorded.returncode, 0)

    def test_wait_stage_cancellation_for_default_ignored_and_blocked_callers(self) -> None:
        cases = [
            ("cancel-default-term", "neutral", (signal.SIGTERM,), {143}),
            ("cancel-ignored-hup", "ignore-hup", (signal.SIGHUP,), {129}),
            ("cancel-blocked-hup", "block-hup", (signal.SIGHUP,), {129}),
            # The contract is the supervisor's first observation, not ordinary
            # signal send order. Either selected signal is therefore valid.
            (
                "cancel-continuous",
                "neutral",
                (signal.SIGTERM, signal.SIGHUP),
                {129, 143},
            ),
        ]
        for case, mode, signals, expected_codes in cases:
            with self.subTest(case=case):
                report = self.work / f"{case}-target.txt"
                ready = self.work / f"{case}-ready.txt"
                storage = self.work / f"{case}-storage"
                command = [
                    str(self.probe),
                    "launch",
                    mode,
                    "0",
                    str(BIN),
                    "--storage",
                    str(storage),
                    "record",
                    case,
                    "--",
                    str(self.probe),
                    "target-wait",
                    str(report),
                    str(ready),
                ]
                recorded = self.runner.signal_when_ready(command, ready, signals)
                self.assertIn(recorded.returncode, expected_codes)

                observed = parse_probe_report(report)
                expected = default_probe_report()
                if mode == "ignore-hup":
                    expected["hup"] = "I"
                if mode == "block-hup":
                    expected["blocked_hup"] = 1
                self.assertEqual(observed, expected)

                run = self.show(storage, case)
                metadata = run["metadata"]
                self.assertEqual(metadata["schema_version"], 3)
                self.assertEqual(metadata["state"], "interrupted")
                self.assertEqual(metadata["completeness"], "incomplete")
                self.assertEqual(run["storage_integrity"]["status"], "verified")
                self.assertTrue(metadata["warnings"])
                self.assert_incomplete_self_diff(storage, case)
                self.assert_storage_lock_reacquired(storage, case)

    def test_version_stage_cancellation_does_not_start_target_or_invent_facts(self) -> None:
        case = "cancel-version-ignored-hup"
        version_ready = self.work / f"{case}-version-ready.txt"
        target_report = self.work / f"{case}-target.txt"
        storage = self.work / f"{case}-storage"
        command = [
            str(self.probe),
            "launch",
            "ignore-hup",
            "0",
            str(BIN),
            "--storage",
            str(storage),
            "record",
            case,
            "--",
            str(self.probe),
            "target",
            str(target_report),
        ]
        recorded = self.runner.signal_when_ready(
            command,
            version_ready,
            (signal.SIGHUP,),
            environment={
                "RUNDELTA_STRACE": str(self.version_backend),
                "RF440_PROBE_MODE": "hang",
                "RF440_READY": str(version_ready),
            },
            observe_ready_pid=True,
        )
        self.assertEqual(recorded.returncode, 129)
        self.assertFalse(target_report.exists(), "target ran after pre-spawn cancellation")

        run = self.show(storage, case)
        metadata = run["metadata"]
        self.assertEqual(metadata["state"], "interrupted")
        self.assertEqual(metadata["completeness"], "incomplete")
        self.assertEqual((metadata["exit_code"], metadata["signal"]), (None, None))
        self.assertEqual(
            (metadata["collector_exit_code"], metadata["collector_signal"]),
            (None, None),
        )
        self.assertIsNone(metadata["target_exit_raw"])
        self.assertEqual(metadata["raw_lines"], 0)
        self.assertEqual(run["events"], [])
        self.assertEqual(run["storage_integrity"]["status"], "verified")
        self.assertTrue(
            any("collector was not started" in warning for warning in metadata["warnings"]),
            metadata["warnings"],
        )
        self.assert_incomplete_self_diff(storage, case)
        self.assert_storage_lock_reacquired(storage, case)

    def test_version_probe_timeout_flood_nonzero_and_signal_fail_before_target(self) -> None:
        cases = [
            ("hang", b"timed out after 5 seconds"),
            ("flood", b"exceeded 65536-byte limit"),
            ("nonzero", b"version probe exited nonzero (23)"),
            ("self-signal", b"version probe terminated by signal 15"),
        ]
        for mode, diagnostic in cases:
            with self.subTest(mode=mode):
                case = f"version-{mode}"
                ready = self.work / f"{case}-ready.txt"
                release = None if mode == "hang" else self.work / f"{case}-go.txt"
                target_report = self.work / f"{case}-target.txt"
                storage = self.work / f"{case}-storage"
                environment = {
                    "RUNDELTA_STRACE": str(self.version_backend),
                    "RF440_PROBE_MODE": mode,
                    "RF440_READY": str(ready),
                }
                if release is not None:
                    environment["RF440_RELEASE"] = str(release)
                command = [
                    str(BIN),
                    "--storage",
                    str(storage),
                    "record",
                    case,
                    "--",
                    str(self.probe),
                    "target",
                    str(target_report),
                ]
                completed = self.runner.signal_when_ready(
                    command,
                    ready,
                    (),
                    environment=environment,
                    release=release,
                    observe_ready_pid=True,
                    timeout=9,
                )
                self.assertEqual(completed.returncode, 2)
                self.assertIn(diagnostic, completed.stderr)
                self.assertFalse(target_report.exists())
                self.assert_no_record_started(storage)
                if mode == "nonzero":
                    self.assertLessEqual(len(completed.stderr), 4_400)
                    self.assertIn(b"BEGIN-STDERR", completed.stderr)
                    self.assertIn(b"[truncated; stderr was 10025 bytes]", completed.stderr)
                    self.assertNotIn(b"END-STDERR", completed.stderr)

    def test_version_probe_spawn_and_pre_exec_fail_before_target(self) -> None:
        for mode in ("spawn", "pre-exec"):
            with self.subTest(mode=mode):
                case = f"version-{mode}"
                storage = self.work / f"{case}-storage"
                target_report = self.work / f"{case}-target.txt"
                command = [str(BIN), "--storage", str(storage), "record", case, "--",
                           str(self.probe), "target", str(target_report)]
                if mode == "pre-exec":
                    completed = self.faults.run(command, storage, "prctl-deny",
                                               environment=self.runner.environment,
                                               cwd=self.work, runner=self.runner)
                    events = (completed.evidence / "events.txt").read_text()
                    self.assertIn("event=pdeath-arm-entry", events)
                    self.assertRegex(events, r"denials=1 barriers=0")
                    diagnostic = b"Operation not permitted"
                else:
                    completed = self.runner.run(command, environment={"RUNDELTA_STRACE": str(self.work / "missing-strace")})
                    diagnostic = b"failed to start supervised strace version probe"
                self.assertEqual(completed.returncode, 2, completed.stderr)
                self.assertIn(diagnostic, completed.stderr)
                self.assertFalse(target_report.exists())
                self.assert_no_record_started(storage)

    def test_signal_setup_rollback_failure_keeps_pending_signal_blocked(self) -> None:
        for mode in ("init-action", "init-rollback"):
            with self.subTest(mode=mode):
                storage = self.work / (mode + "-storage")
                version_ready = self.work / (mode + "-version.txt")
                target_report = self.work / (mode + "-target.txt")
                completed = self.faults.run(
                    [str(self.probe), "launch", "ignore-hup", "0", str(BIN),
                     "--storage", str(storage), "record", mode, "--",
                     str(self.probe), "target", str(target_report)], storage, mode,
                    environment=self.runner.environment | {
                        "RUNDELTA_STRACE": str(self.version_backend),
                        "RF440_PROBE_MODE": "success", "RF440_READY": str(version_ready)},
                    cwd=self.work, runner=self.runner)
                self.assertEqual(completed.returncode, 2, completed.stderr)
                events = (completed.evidence / "events.txt").read_text()
                self.assertIn("event=queued-pending-hup", events)
                self.assertIn(b"make control signal observable", completed.stderr)
                self.assertEqual(completed.stdout, b"")
                self.assertFalse(version_ready.exists(), "signal setup failure ran backend")
                self.assertFalse(target_report.exists(), "signal setup failure ran target")
                self.assert_no_record_started(storage)
                if mode == "init-rollback":
                    self.assertIn(b"signal setup rollback failed", completed.stderr)
                    self.assertIn(b"mask=kept blocked", completed.stderr)
                    self.assertGreater(self.faults.denials(completed), 1)
                else:
                    self.assertNotIn(b"signal setup rollback failed", completed.stderr)
                    self.assertEqual(self.faults.denials(completed), 1)

    def test_version_probe_success_freezes_only_stdout_first_line(self) -> None:
        case = "version-success"
        storage = self.work / f"{case}-storage"
        report = self.work / f"{case}-target.txt"
        completed = self.runner.run(
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                case,
                "--",
                str(self.probe),
                "target",
                str(report),
            ],
            environment={
                "RUNDELTA_STRACE": str(self.version_backend),
                "RF440_PROBE_MODE": "success",
            },
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode(errors="replace"))
        self.assertEqual(parse_probe_report(report), default_probe_report())
        metadata = self.show(storage, case)["metadata"]
        self.assertEqual(metadata["backend"], "fixture-strace 1.0")

    def test_signal_wins_when_version_probe_exit_is_also_ready(self) -> None:
        case = "version-signal-and-exit"
        ready = self.work / f"{case}-ready.txt"
        release = self.work / f"{case}-go.txt"
        target_report = self.work / f"{case}-target.txt"
        storage = self.work / f"{case}-storage"
        completed = self.runner.signal_when_ready(
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                case,
                "--",
                str(self.probe),
                "target",
                str(target_report),
            ],
            ready,
            (signal.SIGHUP,),
            environment={
                "RUNDELTA_STRACE": str(self.version_backend),
                "RF440_PROBE_MODE": "success",
                "RF440_READY": str(ready),
                "RF440_RELEASE": str(release),
            },
            release=release,
            observe_ready_pid=True,
        )
        self.assertEqual(completed.returncode, 129)
        self.assertFalse(target_report.exists())
        metadata = self.show(storage, case)["metadata"]
        self.assertEqual(metadata["state"], "interrupted")
        self.assertEqual(metadata["completeness"], "incomplete")
        self.assertEqual(
            (metadata["collector_exit_code"], metadata["collector_signal"]),
            (None, None),
        )

    def test_handshake_pipes_relocate_actual_low_descriptors(self):
        for slot in range(3):
            with self.subTest(slot=slot):
                case = "pipe-low-" + str(slot)
                work = self.work / case
                work.mkdir(mode=0o700)
                storage, target = work / "records", work / "target.txt"
                result = self.faults.run(
                    [str(BIN), "--storage", str(storage), "record", case, "--",
                     str(self.probe), "target", str(target)], storage, case,
                    environment=self.runner.environment, cwd=self.work, runner=self.runner)
                self.assertEqual(result.returncode, 0, result.stderr)
                events = (result.evidence / "events.txt").read_text()
                self.assertIn(f"event=stdio-closed-before-pipe slot={slot} actual_return=0", events)
                self.assertRegex(events, rf"event=actual-pipe-fds (first={slot} second=\d+|first=\d+ second={slot})")
                import re
                duplicates = re.findall(rf"event=actual-high-dup old={slot} lower_bound=3 actual_return=(\d+)", events)
                self.assertTrue(duplicates)
                self.assertTrue(all(int(value) >= 3 for value in duplicates))
                self.assertIn(f"event=actual-low-close slot={slot} actual_return=0", events)
                self.assertTrue(target.exists())
                metadata = self.show(storage, case)["metadata"]
                self.assertEqual(metadata["state"], "completed")
                self.assertEqual(metadata["exit_code"], 0)

    def conditional_wait_case(self, mode, expected, *, live=False):
        # The child-only pause leaves the owner runnable; the scoped broker's
        # safety kill is a failure, never the product's deadline/exit result.
        work = self.work / ("conditional-" + mode)
        work.mkdir(mode=0o700)
        storage, target_report = work / "records", work / "target.txt"
        environment = self.runner.environment.copy()
        if live:
            environment.update({"RUNDELTA_STRACE": str(self.version_backend),
                                "RF450_COLLECTOR_MODE": "trace-live"})
        recorded = self.faults.run(
            [str(BIN), "--storage", str(storage), "record", mode,
             "--max-duration-ms", "100", "--terminate-grace-ms", "0", "--",
             str(self.probe), "target", str(target_report)], storage, mode,
            environment=environment, cwd=self.work, runner=self.runner)
        self.assertEqual(recorded.returncode, expected, recorded.stderr)
        events = (recorded.evidence / "events.txt").read_text()
        self.assertNotIn("event=controller-deadline-kill", events)
        if mode.startswith("pause-"):
            self.assertIn("event=async-restore-pause", events)
            self.assertIn("pdeath_armed=1", events)
            self.assertFalse(target_report.exists())
        elif mode.startswith("cancel-"):
            # The broker synchronizes cancellation after the armed child
            # reaches its restore stop, while the owner remains runnable.
            self.assertIn("event=async-restore-pause", events)
            self.assertIn("pdeath_armed=1", events)
            self.assertIn("event=controller-signal", events)
            self.assertIn("event=pdeath-arm-success", events)
            self.assertRegex(events, r"event=wait-deny .*flags=1 errno=4")
            self.assertFalse(target_report.exists())
        records = list((storage / "runs").glob("*")) if (storage / "runs").exists() else []
        if mode in ("pause-version", "cancel-version-wait-eintr"):
            self.assertEqual(records, [], "unquiesced preflight must not admit a record")
        elif mode == "pause-formal":
            metadata = self.show(storage, mode)["metadata"]
            self.assertEqual(metadata["state"], "capture_failed")
            self.assertEqual(metadata["completeness"], "incomplete")
            self.assertEqual(metadata["collector_signal"], signal.SIGKILL)
            self.assertTrue(any("duration limit exceeded" in value for value in metadata["warnings"]))
            self.assert_incomplete_self_diff(storage, mode)
        else:
            self.assertEqual(len(records), 1)
            metadata = json.loads((records[0] / "metadata.json").read_text())
            self.assertEqual(metadata["state"], "running")
            self.assertFalse((records[0] / "finalized.json").exists())
            self.assertFalse((records[0] / "events.jsonl").exists())
            self.assertIn(metadata["id"].encode(), recorded.stderr)
            self.assertIn(b"remains unfinalized", recorded.stderr)
            self.assertIn(b"not observed reaped", recorded.stderr)
            if live:
                self.assertIn("event=cleanup-deny", events)
                self.assertIn("injected_return=-1", events)
                self.assertGreater((records[0] / "raw/trace.log").stat().st_size, 0)
                self.assertFalse(target_report.exists(), "synthetic producer must not execute target")
            else:
                self.assertTrue(target_report.exists())
                self.assertRegex(events, r"event=wait-deny .*flags=1 errno=4")

    def test_version_pipe_eintr_cannot_outlive_preflight_deadline(self):
        case = "version-read-eintr"
        storage = self.work / (case + "-storage")
        target = self.work / (case + "-target.txt")
        result = self.faults.run(
            [str(BIN), "--storage", str(storage), "record", case, "--",
             str(self.probe), "target", str(target)], storage, case,
            environment=self.runner.environment | {
                "RUNDELTA_STRACE": str(self.version_backend), "RF440_PROBE_MODE": "success"},
            cwd=self.work, runner=self.runner)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(b"timed out after 5 seconds", result.stderr)
        events = (result.evidence / "events.txt").read_text()
        self.assertRegex(events, r"event=version-pipe-read-deny .*bytes=8192 errno=4 resource=pipe:\[")
        self.assertNotIn("event=controller-deadline-kill", events)
        self.assertFalse(target.exists())
        self.assert_no_record_started(storage)

    def signal_leaf_case(self, mode, diagnostic):
        storage = self.work / (mode + "-storage")
        target = self.work / (mode + "-target.txt")
        result = self.faults.run(
            [str(BIN), "--storage", str(storage), "record", mode, "--",
             str(self.probe), "target", str(target)], storage, mode,
            environment=self.runner.environment | {
                "RUNDELTA_STRACE": str(self.version_backend), "RF440_PROBE_MODE": "success"},
            cwd=self.work, runner=self.runner)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(diagnostic, result.stderr)
        events = (result.evidence / "events.txt").read_text()
        event = "signal-read-deny" if mode == "signal-read-eintr" else "signal-poll-deny"
        self.assertIn("event=" + event, events)
        self.assertNotIn("event=controller-deadline-kill", events)
        self.assertFalse(target.exists())
        self.assert_no_record_started(storage)

    def test_signalfd_eintr_reports_incomplete_observation_without_spinning(self):
        self.signal_leaf_case("signal-read-eintr", b"read recorder signalfd")

    def test_finite_poll_eintr_returns_at_original_preflight_deadline(self):
        self.signal_leaf_case("signal-poll-eintr", b"timed out after 5 seconds")

    def test_version_exec_handshake_has_parent_side_deadline(self):
        self.conditional_wait_case("pause-version", 2)

    def test_formal_exec_handshake_preserves_duration_limit_facts(self):
        self.conditional_wait_case("pause-formal", 125)

    def test_final_wait_eintr_is_bounded_and_keeps_unfinalized_record(self):
        self.conditional_wait_case("reap-eintr", 2)

    def test_cancelled_unquiesced_preflight_cannot_finalize_record(self):
        self.conditional_wait_case("cancel-version-wait-eintr", 2)

    def test_refused_kill_and_nonready_reap_cannot_freeze_live_raw(self):
        self.conditional_wait_case("reap-zero-kill-deny", 2, live=True)

    def test_parent_death_is_armed_before_child_signal_restore(self) -> None:
        # Stop the actual fork child at its first HUP restoration after observing
        # successful PR_SET_PDEATHSIG. The root owner is killed through a pidfd;
        # the test broker is the child's subreaper and records its real SIGKILL.
        work = self.work / "external-parent-death"
        work.mkdir(mode=0o700)
        ready, release = work / "ready", work / "release"
        storage, target_report = work / "records", work / "target.txt"
        backend_ready = work / "backend-ready"
        os.mkfifo(release, 0o600)
        gate = os.open(release, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
        folder, argv, environment = self.faults.prepare(
            [str(self.probe), "launch", "neutral", "0", str(BIN), "--storage", str(storage),
             "record", "pre-exec", "--", str(self.probe), "target", str(target_report)],
            storage, "child-restore-barrier", self.runner.environment | {
                "CAPTURE_FAULT_READY": str(ready), "CAPTURE_FAULT_RELEASE": str(release),
                "RUNDELTA_STRACE": str(self.version_backend), "RF440_PROBE_MODE": "success",
                "RF440_READY": str(backend_ready)}, work)
        process = None
        pidfd = None
        move_watch = None
        start = time.monotonic_ns()
        try:
            move_watch = watch_for_atomic_move(work)
            process = subprocess.Popen(argv, cwd=work, env=environment,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            deadline = time.monotonic() + 10
            while not ready.exists():
                self.assertIsNone(process.poll(), "child barrier was not reached")
                self.assertLess(time.monotonic(), deadline, "pre-exec child barrier timed out")
                time.sleep(0.005)
            owner, child = parse_owner_child(ready.read_text(encoding="ascii"))
            wait_for_atomic_move(move_watch, ready.name, deadline)
            pidfd = os.pidfd_open(owner)
            signal.pidfd_send_signal(pidfd, signal.SIGKILL)
            os.write(gate, b"x")
            output, errors = process.communicate(timeout=10)
            result = self.faults.result(folder, subprocess.CompletedProcess(argv, process.returncode, output, errors),
                                       time.monotonic_ns() - start)
            self.assertEqual(result.returncode, -signal.SIGKILL)
            log = (folder / "events.txt").read_text()
            self.assertIn(f"event=pdeath-arm-success pid={child} return=0", log)
            self.assertIn(f"event=restore-barrier owner={owner} child={child} pdeath_armed=1", log)
            self.assertRegex(log, rf"event=child-exit pid={child} owner=0 .*signal=9")
            self.assertFalse(Path(f"/proc/{owner}").exists())
            self.assertFalse(Path(f"/proc/{child}").exists())
            self.assertFalse(backend_ready.exists())
            self.assertFalse(target_report.exists())
        finally:
            if process is not None and process.poll() is None:
                os.write(gate, b"x")
                process.communicate(timeout=5)
            if pidfd is not None:
                os.close(pidfd)
            if move_watch is not None:
                os.close(move_watch)
            os.close(gate)


    def test_capture_syscall_ready_refuses_existing_destination(self) -> None:
        work = self.work / "ready-existing-destination"
        work.mkdir(mode=0o700)
        ready, release = work / "ready", work / "release"
        ready.write_text("preserve\n", encoding="ascii")
        storage, target_report = work / "records", work / "target.txt"
        os.mkfifo(release, 0o600)
        gate = os.open(release, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
        folder, argv, environment = self.faults.prepare(
            [str(self.probe), "launch", "neutral", "0", str(BIN), "--storage", str(storage),
             "record", "pre-exec-existing-ready", "--", str(self.probe), "target", str(target_report)],
            storage, "child-restore-barrier", self.runner.environment | {
                "CAPTURE_FAULT_READY": str(ready), "CAPTURE_FAULT_RELEASE": str(release),
                "RUNDELTA_STRACE": str(self.version_backend), "RF440_PROBE_MODE": "success"}, work)
        process = None
        try:
            process = subprocess.Popen(argv, cwd=work, env=environment,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            process.communicate(timeout=10)
            self.assertEqual(process.returncode, 2)
            self.assertEqual(ready.read_text(encoding="ascii"), "preserve\n")
            self.assertFalse(target_report.exists())
        finally:
            if process is not None and process.poll() is None:
                try:
                    os.write(gate, b"x")
                except OSError:
                    pass
                try:
                    process.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate(timeout=3)
            os.close(gate)


    def test_parent_sigkill_triggers_version_probe_pdeathsig(self) -> None:
        case = "version-parent-sigkill"
        ready = self.work / f"{case}-ready.txt"
        storage = self.work / f"{case}-storage"
        target_report = self.work / f"{case}-target.txt"
        environment = self.runner.environment.copy()
        environment.update(
            {
                "RUNDELTA_STRACE": str(self.version_backend),
                "RF440_PROBE_MODE": "hang",
                "RF440_READY": str(ready),
            }
        )
        process = subprocess.Popen(
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                case,
                "--",
                str(self.probe),
                "target",
                str(target_report),
            ],
            cwd=self.work,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        probe_pidfd: int | None = None
        try:
            deadline = time.monotonic() + 3
            while not ready.exists():
                self.assertIsNone(process.poll(), "recorder exited before probe readiness")
                self.assertLess(time.monotonic(), deadline, "probe readiness timeout")
                time.sleep(0.01)
            probe_pid = parse_ready_pid(ready.read_text(encoding="ascii"))
            probe_pidfd = os.pidfd_open(probe_pid)
            process.kill()
            self.assertEqual(process.wait(timeout=3), -signal.SIGKILL)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    signal.pidfd_send_signal(probe_pidfd, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.01)
            else:
                self.fail("version probe survived recorder SIGKILL")
            process.communicate(timeout=2)
            self.assertFalse(target_report.exists())
            self.assert_no_record_started(storage)
        finally:
            if process.poll() is None:
                self.runner._signal_group(process.pid, signal.SIGKILL)
                process.wait(timeout=3)
            if probe_pidfd is not None:
                try:
                    signal.pidfd_send_signal(probe_pidfd, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.close(probe_pidfd)

    def test_pthread_eagain_does_not_affect_single_thread_supervision(self) -> None:
        case = "pthread-eagain"
        storage = self.work / f"{case}-storage"
        report = self.work / f"{case}-target.txt"
        recorded = self.runner.run(
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                case,
                "--",
                str(self.probe),
                "target",
                str(report),
            ],
            environment={"LD_PRELOAD": str(self.pthread_eagain)},
        )
        self.assertEqual(recorded.returncode, 0, recorded.stderr.decode(errors="replace"))
        self.assertEqual(parse_probe_report(report), default_probe_report())
        metadata = self.show(storage, case)["metadata"]
        self.assertEqual(metadata["state"], "completed")
        self.assertEqual(metadata["completeness"], "complete_for_supported_events")
        self.assertEqual(metadata["warnings"], [])

    def host_isolation_case(self, phase):
        storage = self.work / (phase + "-host-storage")
        target = self.work / (phase + "-host-target")
        report = self.work / (phase + "-host-state")
        constructors = self.work / (phase + "-constructors")
        recorded = self.faults.run(
            [str(BIN), "--storage", str(storage), "record", phase, "--",
             "/usr/bin/touch", str(target)], storage,
            environment=self.runner.environment | {
                "LD_PRELOAD": str(self.extra_thread_probe),
                "RD_THREAD_GUARD_RECORDER": str(BIN), "RD_THREAD_GUARD_REPORT": str(report),
                "RD_THREAD_GUARD_PHASE": phase, "RD_HOST_CTORS": str(constructors)},
            cwd=self.work, runner=self.runner)
        self.assertEqual(recorded.returncode, 0, recorded.stderr)
        facts = dict(field.split("=") for field in report.read_text().split())
        self.assertGreaterEqual(int(facts["tasks"]), 2, "host worker was never created")
        self.assertEqual(facts["worker"], "1")
        self.assertEqual(facts["reaper"], "1" if phase == "reaper-gap" else "0")
        events = (recorded.evidence / "events.txt").read_text()
        owner_pids = set(__import__('re').findall(r"event=exec execution=\d+ pid=(\d+)", events))
        self.assertEqual(owner_pids, {facts["owner"]}, "capture exec changed the CLI PID")
        images = [dict(field.split("=", 1) for field in line.split())
                  for line in constructors.read_text().splitlines()]
        self.assertEqual(sum(row["pid"] == facts["owner"] for row in images), 1,
                         "host loader module re-entered the clean owner")
        self.assertTrue(any(row["image"] == "/usr/bin/touch" for row in images),
                        "cleaning owner loaders silently removed target environment")
        self.assertTrue(target.exists())
        self.assertEqual(self.show(storage, phase)["metadata"]["state"], "completed")

    def test_existing_extra_thread_is_replaced_before_capture_side_effects(self):
        self.host_isolation_case("initial")

    def test_late_host_thread_is_destroyed_at_owner_exec(self):
        self.host_isolation_case("lock-gap")

    def test_late_host_reaper_is_destroyed_at_owner_exec(self):
        self.host_isolation_case("reaper-gap")

    def test_capture_limit_help_defaults_and_metadata_schema_remain_compatible(self) -> None:
        help_output = self.runner.run([str(BIN), "record", "--help"])
        self.assertEqual(help_output.returncode, 0)
        for option in (
            b"--max-duration-ms",
            b"--max-trace-bytes",
            b"--terminate-grace-ms",
        ):
            self.assertIn(option, help_output.stdout)
        self.assertIn(b"default: 2000", help_output.stdout)

        recorded, observed, run = self.record("capture-limits-default", "neutral")
        self.assertEqual(recorded.returncode, 0)
        self.assertEqual(observed, default_probe_report())
        metadata = run["metadata"]
        self.assertEqual(metadata["schema_version"], 3)
        self.assertEqual(
            set(metadata),
            {
                "schema_version",
                "tool_version",
                "id",
                "name",
                "command",
                "cwd",
                "started",
                "ended",
                "backend",
                "state",
                "exit_code",
                "signal",
                "warnings",
                "parsed_events",
                "raw_lines",
                "environment",
                "completeness",
                "collector_exit_code",
                "collector_signal",
                "target_exit_raw",
            },
        )

    def test_duration_limit_commits_incomplete_record_and_reaps_target_tree(self) -> None:
        case = "capture-duration-limit"
        storage = self.work / f"{case}-storage"
        report = self.work / f"{case}-target.txt"
        ready = self.work / f"{case}-ready.txt"
        # This fixture stays in the recorder's private session. The runner's
        # protected group inspection rejects a live target before cleanup; a
        # post-run PID lookup could instead observe an unrelated recycled ID.
        completed = self.runner.run(
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                case,
                "--max-duration-ms",
                "500",
                "--terminate-grace-ms",
                "50",
                "--",
                str(self.probe),
                "target-wait",
                str(report),
                str(ready),
            ],
            timeout=5,
        )
        self.assertEqual(completed.returncode, 125, completed.stderr.decode(errors="replace"))
        self.assertTrue(report.exists(), "target did not start before duration boundary")
        self.assertTrue(ready.exists(), "target did not publish its pid before duration boundary")

        run = self.show(storage, case)
        metadata = run["metadata"]
        self.assertEqual(metadata["schema_version"], 3)
        self.assertEqual(metadata["state"], "capture_failed")
        self.assertEqual(metadata["completeness"], "incomplete")
        self.assertTrue(
            any("capture duration limit exceeded" in warning for warning in metadata["warnings"]),
            metadata["warnings"],
        )
        self.assertEqual(run["storage_integrity"]["status"], "verified")
        self.assert_incomplete_self_diff(storage, case)
        self.assert_storage_lock_reacquired(storage, case)

    def test_trace_byte_limit_is_strict_and_skips_oversized_raw_parse(self) -> None:
        for size, exceeded in ((64, False), (65, True)):
            with self.subTest(size=size):
                case = f"capture-trace-bytes-{size}"
                storage = self.work / f"{case}-storage"
                target_report = self.work / f"{case}-target.txt"
                completed = self.runner.run(
                    [
                        str(BIN),
                        "--storage",
                        str(storage),
                        "record",
                        case,
                        "--max-trace-bytes",
                        "64",
                        "--terminate-grace-ms",
                        "20",
                        "--",
                        str(self.probe),
                        "target",
                        str(target_report),
                    ],
                    environment={
                        "RUNDELTA_STRACE": str(self.version_backend),
                        "RF440_PROBE_MODE": "success",
                        "RF450_COLLECTOR_MODE": "trace-exit",
                        "RF450_TRACE_BYTES": str(size),
                    },
                )
                self.assertEqual(
                    completed.returncode,
                    125,
                    completed.stderr.decode(errors="replace"),
                )
                self.assertFalse(target_report.exists(), "fixture unexpectedly ran target")
                run = self.show(storage, case)
                metadata = run["metadata"]
                limit_warnings = [
                    warning
                    for warning in metadata["warnings"]
                    if "capture trace byte limit exceeded" in warning
                ]
                if exceeded:
                    self.assertEqual(
                        limit_warnings,
                        [
                            "capture trace byte limit exceeded (observed: 65 bytes, limit: 64 bytes)"
                        ],
                    )
                    self.assertEqual(metadata["raw_lines"], 0)
                    self.assertEqual(metadata["parsed_events"], 0)
                    self.assertEqual(run["events"], [])
                else:
                    self.assertEqual(limit_warnings, [])
                    self.assertEqual(metadata["raw_lines"], 1)
                self.assertEqual(metadata["state"], "capture_failed")
                self.assertEqual(metadata["completeness"], "incomplete")
                self.assertEqual(run["storage_integrity"]["status"], "verified")
                self.assert_incomplete_self_diff(storage, case)
                if exceeded:
                    self.assert_storage_lock_reacquired(storage, case)

    def test_streaming_late_invalid_utf8_discards_valid_prefix(self) -> None:
        case = "capture-stream-late-invalid-utf8"
        storage = self.work / f"{case}-storage"
        target_report = self.work / f"{case}-target.txt"
        completed = self.runner.run(
            [
                str(BIN),
                "--storage",
                str(storage),
                "record",
                case,
                "--",
                str(self.probe),
                "target",
                str(target_report),
            ],
            environment={
                "RUNDELTA_STRACE": str(self.version_backend),
                "RF440_PROBE_MODE": "success",
                "RF450_COLLECTOR_MODE": "late-invalid-utf8",
            },
        )
        self.assertEqual(completed.returncode, 125, completed.stderr.decode(errors="replace"))
        self.assertFalse(target_report.exists(), "fixture unexpectedly ran target")

        run = self.show(storage, case)
        metadata = run["metadata"]
        self.assertEqual(metadata["state"], "capture_failed")
        self.assertEqual(metadata["completeness"], "incomplete")
        self.assertEqual(metadata["raw_lines"], 0)
        self.assertEqual(metadata["parsed_events"], 0)
        self.assertEqual((metadata["exit_code"], metadata["signal"]), (None, None))
        self.assertEqual(metadata["target_exit_raw"], None)
        self.assertEqual(
            (metadata["collector_exit_code"], metadata["collector_signal"]),
            (0, None),
        )
        self.assertEqual(run["events"], [])
        self.assertTrue(
            any(
                warning.startswith("cannot read trace:")
                and "valid UTF-8" in warning
                for warning in metadata["warnings"]
            ),
            metadata["warnings"],
        )
        self.assertFalse(
            any("/tool" in warning for warning in metadata["warnings"]),
            metadata["warnings"],
        )
        self.assertEqual(run["storage_integrity"]["status"], "verified")
        self.assert_incomplete_self_diff(storage, case)

    def test_trace_limit_escalates_term_and_recorder_signal_retains_priority(self) -> None:
        cases = [
            ("kill", (), 125, "capture_failed"),
            ("signal", (signal.SIGHUP,), 129, "interrupted"),
        ]
        for mode, recorder_signals, expected_code, expected_state in cases:
            with self.subTest(mode=mode):
                case = f"capture-trace-limit-{mode}"
                storage = self.work / f"{case}-storage"
                term_ready = self.work / f"{case}-term-ready.txt"
                target_report = self.work / f"{case}-target.txt"
                completed = self.runner.signal_when_ready(
                    [
                        str(BIN),
                        "--storage",
                        str(storage),
                        "record",
                        case,
                        "--max-trace-bytes",
                        "0",
                        "--terminate-grace-ms",
                        "1000" if recorder_signals else "80",
                        "--",
                        str(self.probe),
                        "target",
                        str(target_report),
                    ],
                    term_ready,
                    recorder_signals,
                    environment={
                        "RUNDELTA_STRACE": str(self.version_backend),
                        "RF440_PROBE_MODE": "success",
                        "RF450_COLLECTOR_MODE": "trace-hang",
                        "RF450_TRACE_BYTES": "1",
                        "RF450_TERM_READY": str(term_ready),
                    },
                    observe_ready_pid=True,
                    timeout=5,
                )
                self.assertEqual(
                    completed.returncode,
                    expected_code,
                    completed.stderr.decode(errors="replace"),
                )
                self.assertFalse(target_report.exists(), "fixture unexpectedly ran target")
                metadata = self.show(storage, case)["metadata"]
                self.assertEqual(metadata["state"], expected_state)
                self.assertEqual(metadata["completeness"], "incomplete")
                self.assertTrue(
                    any(
                        warning
                        == "capture trace byte limit exceeded (observed: 1 bytes, limit: 0 bytes)"
                        for warning in metadata["warnings"]
                    ),
                    metadata["warnings"],
                )
                if recorder_signals:
                    self.assertEqual(metadata["collector_signal"], signal.SIGHUP)
                else:
                    self.assertEqual(metadata["collector_signal"], signal.SIGKILL)
                self.assert_incomplete_self_diff(storage, case)

    def test_sigpipe_current_rust_command_boundary(self) -> None:
        # Direct exec distinguishes DFL from IGN. The target spawned through the
        # current recorder/strace path observes DFL in both cases. This freezes the
        # DEC-04/RF-720 boundary rather than endorsing it as transparency.
        for mode, expected_direct_pipe in [
            ("pipe-default", "D"),
            ("pipe-ignore", "I"),
        ]:
            with self.subTest(mode=mode):
                case = mode
                _, direct = self.direct(case, mode)
                recorded, observed, _ = self.record(case, mode)
                self.assertEqual(direct["pipe"], expected_direct_pipe)
                self.assertEqual(observed["pipe"], "D")
                self.assertEqual(
                    {key: observed[key] for key in ("hup", "int", "term")},
                    {"hup": "D", "int": "D", "term": "D"},
                )
                self.assertEqual(recorded.returncode, 0)

    def test_closed_standard_descriptors_are_reopened_at_current_record_boundary(self) -> None:
        # The probe samples 0/1/2 before opening its report. Direct exec retains
        # each closed descriptor. The current Rust/Command/strace path presents
        # all three as open. A future native shim must update these expectations.
        for close_mask in (1, 2, 4, 7):
            with self.subTest(close_mask=close_mask):
                case = f"closed-{close_mask}"
                _, direct = self.direct(case, "neutral", close_mask)
                recorded, observed, run = self.record(case, "neutral", close_mask)
                self.assertEqual(
                    {f"fd{descriptor}": direct[f"fd{descriptor}"] for descriptor in range(3)},
                    {
                        f"fd{descriptor}": 0 if close_mask & (1 << descriptor) else 1
                        for descriptor in range(3)
                    },
                )
                self.assertEqual(
                    {f"fd{descriptor}": observed[f"fd{descriptor}"] for descriptor in range(3)},
                    {"fd0": 1, "fd1": 1, "fd2": 1},
                )
                # Assert committed target facts before the recorder process
                # status: stderr closure and summary I/O are a separate outcome.
                metadata = run["metadata"]
                self.assertEqual((metadata["exit_code"], metadata["signal"]), (0, None))
                self.assertEqual(run["storage_integrity"]["status"], "verified")
                self.assertEqual(
                    recorded.returncode,
                    0,
                    "current summary/output boundary changed; do not reinterpret the committed target outcome",
                )


if __name__ == "__main__":
    main()
