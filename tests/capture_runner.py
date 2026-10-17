#!/usr/bin/env python3
"""Standalone safety regressions for the capture acceptance process runner.

These tests need Linux pidfds, but no strace or RunDelta build. Numeric group
operations are intercepted before reaching the kernel if the original direct
child is no longer waitable. Independent pidfds bound all test cleanup.
"""

from __future__ import annotations

from contextlib import ExitStack
import errno
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import unittest
from unittest import mock

from runtime_boundary import IsolatedRunner
import runtime_boundary
from acceptance_support import main, require_owned_group_context


REAL_POPEN = subprocess.Popen
REAL_PIDFD_OPEN = os.pidfd_open
REAL_WAITID = os.waitid
REAL_KILLPG = os.killpg
REAL_PIDFD_SEND_SIGNAL = signal.pidfd_send_signal


class OwnedProcesses:
    """Hold stable cleanup handles and audit the runner's group authority."""

    def __init__(self) -> None:
        self.children: list[tuple[subprocess.Popen, int | None]] = []
        self.numeric_operations: list[tuple[int, int]] = []
        self.cleanup = ExitStack()

    def spawn(self, *args, **kwargs) -> subprocess.Popen:
        require_owned_group_context()
        if not kwargs.get("start_new_session"):
            raise ValueError("the test adapter requires a private process session")
        process = REAL_POPEN(*args, **kwargs)
        self.children.append((process, None))
        resources = ExitStack()
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                resources.callback(stream.close)
        try:
            descriptor = REAL_PIDFD_OPEN(process.pid)
        except BaseException:
            try:
                self.stop(process, None)
            finally:
                resources.close()
            raise
        self.children[-1] = (process, descriptor)
        resources.callback(os.close, descriptor)
        resources.callback(self.stop, process, descriptor)
        self.cleanup.callback(resources.close)
        return process

    @staticmethod
    def stop(process: subprocess.Popen, descriptor: int | None) -> None:
        if descriptor is None:
            # The default disposition and absence of a competing reaper keep
            # the private group protected until this final wait. ECHILD ends
            # numeric cleanup authority even when acquisition itself failed.
            REAL_WAITID(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            try:
                REAL_KILLPG(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            try:
                REAL_PIDFD_SEND_SIGNAL(descriptor, signal.SIGKILL)
            except ProcessLookupError:
                pass
        # A refused signal cannot establish that a still-live child is ready to
        # reap. Propagate that failure so the stack attempts the other resources.
        process.wait(timeout=3)

    def group_operation(self, group: int, number: int) -> None:
        try:
            REAL_WAITID(os.P_PID, group, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except ChildProcessError as error:
            raise AssertionError(
                "numeric group operation after leader ownership was released"
            ) from error
        self.numeric_operations.append((group, number))
        REAL_KILLPG(group, number)

    def close(self) -> None:
        # Each child's kill/reap, handle, and streams are independent callbacks;
        # one cleanup error cannot skip the remaining test-owned resources.
        self.cleanup.close()


@unittest.skipUnless(
    sys.platform == "linux" and hasattr(os, "pidfd_open")
    and hasattr(signal, "pidfd_send_signal") and hasattr(os, "WNOWAIT"),
    "Linux pidfds and WNOWAIT are required",
)
class CaptureRunner(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="capture-runner-")
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        self.runner = IsolatedRunner(self.work, dict(os.environ))
        self.owned = OwnedProcesses()
        self.addCleanup(self.owned.close)
        self.spawn_patch = mock.patch.object(
            runtime_boundary.subprocess, "Popen", side_effect=self.owned.spawn
        )
        self.group_patch = mock.patch.object(
            runtime_boundary.os, "killpg", side_effect=self.owned.group_operation
        )
        self.spawn_patch.start()
        self.group_patch.start()
        self.addCleanup(self.spawn_patch.stop)
        self.addCleanup(self.group_patch.stop)

    @staticmethod
    def command(source: str, *arguments: Path) -> list[str]:
        return [sys.executable, "-c", source, *map(str, arguments)]

    def test_normal_output_and_exit_preserve_group_authority(self) -> None:
        result = self.runner.run(self.command(
            "import sys; sys.stdout.buffer.write(b'out\\xff\\n'); "
            "sys.stderr.buffer.write(b'err\\xfe\\n'); sys.exit(7)"
        ))
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (7, b"out\xff\n", b"err\xfe\n"))

    def test_ready_signal_preserves_output_and_group_authority(self) -> None:
        ready = self.work / "ready"
        source = (
            "import os, pathlib, sys, time; "
            "print('ready output', flush=True); "
            "pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); "
            "time.sleep(30)"
        )
        result = self.runner.signal_when_ready(
            self.command(source, ready), ready, (signal.SIGTERM,), timeout=2
        )
        self.assertEqual(result.returncode, -signal.SIGTERM)
        self.assertEqual(result.stdout, b"ready output\n")

    def observe_tree(self, ready: Path, release: Path) -> tuple[list[int], threading.Thread, list[BaseException]]:
        descriptors: list[int] = []
        failures: list[BaseException] = []

        def observe() -> None:
            try:
                deadline = time.monotonic() + 2
                while True:
                    if time.monotonic() >= deadline:
                        raise AssertionError("test descendant readiness timeout")
                    try:
                        contents = ready.read_text(encoding="ascii")
                        fields = contents[:-1].split(" ") if contents.endswith("\n") and contents.count("\n") == 1 else []
                        numbers = [int(value) for value in fields]
                        if (len(fields) != 2 or any(not value.isdecimal() or value[0] == "0" for value in fields)
                                or any(not value.isascii() for value in fields)
                                or any(number <= 0 for number in numbers)
                                or numbers[0] == numbers[1]):
                            numbers = []
                    except (FileNotFoundError, ValueError):
                        numbers = []
                    if len(numbers) == 2 and all(number > 0 for number in numbers) and numbers[0] != numbers[1]:
                        # The leader waits for this release, so no saved PID can
                        # be recycled before both stable observation handles exist.
                        acquired: list[int] = []
                        try:
                            for number in numbers:
                                acquired.append(REAL_PIDFD_OPEN(number))
                            if time.monotonic() >= deadline:
                                raise AssertionError("test descendant readiness timeout")
                        except BaseException:
                            for descriptor in acquired:
                                os.close(descriptor)
                            raise
                        descriptors.extend(acquired)
                        release.write_text("go")
                        return
                    time.sleep(0.005)
            except BaseException as error:
                failures.append(error)

        thread = threading.Thread(target=observe, daemon=True)
        thread.start()

        def cleanup() -> None:
            thread.join(timeout=3)
            for descriptor in descriptors:
                try:
                    signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                finally:
                    try:
                        os.close(descriptor)
                    except OSError as error:
                        if error.errno != errno.EBADF:
                            raise

        self.addCleanup(cleanup)
        return descriptors, thread, failures

    def tree_command(self, ready: Path, release: Path, *, exit_leader: bool) -> list[str]:
        return self.command(
            "import os, pathlib, sys, tempfile, time\n"
            "child = os.fork()\n"
            "if child == 0:\n"
            " print('descendant output', flush=True)\n"
            " time.sleep(30)\n"
            " os._exit(0)\n"
            "ready = pathlib.Path(sys.argv[1])\n"
            "with tempfile.NamedTemporaryFile(mode='w', dir=ready.parent, delete=False) as temporary:\n"
            " temporary.write(f'{os.getpid()} {child}\\n')\n"
            "os.replace(temporary.name, ready)\n"
            "while not pathlib.Path(sys.argv[2]).exists(): time.sleep(.005)\n"
            + ("os._exit(0)\n" if exit_leader else "time.sleep(30)\n"),
            ready, release,
        )

    def check_tree_stopped(self, descriptors: list[int], thread: threading.Thread,
                           failures: list[BaseException]) -> None:
        thread.join(timeout=3)
        self.assertFalse(thread.is_alive(), "test observer did not finish")
        self.assertEqual(failures, [])
        self.assertEqual(len(descriptors), 2)
        for descriptor in descriptors:
            self.assertTrue(select.select([descriptor], [], [], 1)[0],
                            "original leader or descendant survived cleanup")

    def test_observer_waits_for_complete_ready_pid_pair_before_release(self) -> None:
        ready, release = self.work / "partial-ready", self.work / "partial-release"
        ready.write_text("")
        first_invalid_read = threading.Event()
        invalid_processed = threading.Event()
        continue_invalid_read = threading.Event()
        allow_complete_read = threading.Event()
        original_read_text = Path.read_text
        opened: list[int] = []
        descriptors: list[int] = []
        thread: threading.Thread | None = None

        def observe_read(path: Path, *args, **kwargs) -> str:
            sample = original_read_text(path, *args, **kwargs)
            if path == ready and not first_invalid_read.is_set():
                first_invalid_read.set()
                if not continue_invalid_read.wait(timeout=2):
                    raise AssertionError("test did not release first incomplete readiness sample")
                return sample
            if path == ready and first_invalid_read.is_set() and not invalid_processed.is_set():
                invalid_processed.set()
                if not allow_complete_read.wait(timeout=2):
                    raise AssertionError("test did not publish complete readiness pair")
                return original_read_text(path, *args, **kwargs)
            return sample

        def open_fake_pidfd(number: int) -> int:
            opened.append(number)
            return os.open(os.devnull, os.O_RDONLY)

        signal_patch = mock.patch.object(signal, "pidfd_send_signal")
        signal_patch.start()
        self.addCleanup(signal_patch.stop)
        pidfd_patch = mock.patch(f"{__name__}.REAL_PIDFD_OPEN", side_effect=open_fake_pidfd)
        pidfd_patch.start()
        self.addCleanup(pidfd_patch.stop)
        try:
            with mock.patch.object(Path, "read_text", observe_read):
                descriptors, thread, failures = self.observe_tree(ready, release)
                self.assertTrue(first_invalid_read.wait(timeout=1), "observer did not read empty readiness")
                continue_invalid_read.set()
                self.assertTrue(invalid_processed.wait(timeout=1), "observer did not process empty readiness")
                self.assertFalse(release.exists(), "observer released after an incomplete readiness sample")
                ready.write_text("123 456\n")
                allow_complete_read.set()
                thread.join(timeout=2)
            self.assertFalse(thread.is_alive(), "observer did not finish after complete readiness")
            self.assertEqual(failures, [])
            self.assertEqual(opened, [123, 456])
            self.assertEqual(len(descriptors), 2)
            self.assertTrue(release.exists(), "observer did not release after acquiring both pidfds")
        finally:
            continue_invalid_read.set()
            allow_complete_read.set()
            if thread is not None:
                thread.join(timeout=3)
            for descriptor in descriptors:
                try:
                    os.close(descriptor)
                except OSError as error:
                    if error.errno != errno.EBADF:
                        raise

    def test_observer_rejects_noncanonical_and_partial_pid_pairs(self) -> None:
        signal_patch = mock.patch.object(signal, "pidfd_send_signal")
        signal_patch.start()
        self.addCleanup(signal_patch.stop)
        for index, invalid in enumerate(("+123 456\n", "00123 456\n", "123 456")):
            with self.subTest(invalid=invalid):
                ready = self.work / f"invalid-pair-{index}.ready"
                release = self.work / f"invalid-pair-{index}.release"
                ready.write_text(invalid)
                second_read = threading.Event()
                continue_read = threading.Event()
                original_read_text = Path.read_text
                opened: list[int] = []
                descriptors: list[int] = []
                thread: threading.Thread | None = None
                reads = 0

                def observe_read(path: Path, *args, **kwargs) -> str:
                    nonlocal reads
                    sample = original_read_text(path, *args, **kwargs)
                    if path == ready:
                        reads += 1
                        if reads == 2:
                            second_read.set()
                            if not continue_read.wait(timeout=2):
                                raise AssertionError("test did not continue invalid PID observation")
                    return sample

                def open_fake_pidfd(number: int) -> int:
                    opened.append(number)
                    return os.open(os.devnull, os.O_RDONLY)

                pidfd_patch = mock.patch(f"{__name__}.REAL_PIDFD_OPEN", side_effect=open_fake_pidfd)
                pidfd_patch.start()
                try:
                    with mock.patch.object(Path, "read_text", observe_read):
                        descriptors, thread, failures = self.observe_tree(ready, release)
                        self.assertTrue(second_read.wait(timeout=1), "observer did not process the invalid pair")
                        self.assertFalse(release.exists(), "observer released on a noncanonical/partial PID pair")
                        self.assertEqual(opened, [], "observer opened a pidfd for an invalid PID pair")
                        ready.write_text("123 456\n")
                        continue_read.set()
                        thread.join(timeout=2)
                    self.assertFalse(thread.is_alive(), "observer did not finish after valid PID publication")
                    self.assertEqual(failures, [])
                    self.assertEqual(opened, [123, 456])
                    self.assertTrue(release.exists(), "observer did not release after the valid PID pair")
                finally:
                    continue_read.set()
                    if thread is not None:
                        thread.join(timeout=3)
                    for descriptor in descriptors:
                        try:
                            os.close(descriptor)
                        except OSError as error:
                            if error.errno != errno.EBADF:
                                raise
                    pidfd_patch.stop()

    def test_observer_second_pidfd_failure_closes_first_before_release(self) -> None:
        ready, release = self.work / "pidfd-failure-ready", self.work / "pidfd-failure-release"
        ready.write_text("123 456\n")
        opened: list[int] = []
        descriptors: list[int] = []
        thread: threading.Thread | None = None

        def fail_second_pidfd(number: int) -> int:
            if number == 456:
                raise OSError(errno.EMFILE, "injected second pidfd exhaustion")
            descriptor = os.open(os.devnull, os.O_RDONLY)
            opened.append(descriptor)
            return descriptor

        pidfd_patch = mock.patch(f"{__name__}.REAL_PIDFD_OPEN", side_effect=fail_second_pidfd)
        pidfd_patch.start()

        try:
            descriptors, thread, failures = self.observe_tree(ready, release)
            thread.join(timeout=2)
            self.assertFalse(thread.is_alive(), "observer did not finish after pidfd failure")
            self.assertEqual(len(failures), 1)
            self.assertIsInstance(failures[0], OSError)
            self.assertEqual(len(opened), 1)
            self.assertEqual(descriptors, [])
            self.assertFalse(release.exists(), "observer released after second pidfd acquisition failed")
            with self.assertRaises(OSError) as closed:
                os.fstat(opened[0])
            self.assertEqual(closed.exception.errno, errno.EBADF)
        finally:
            if thread is not None:
                thread.join(timeout=3)
            for descriptor in opened:
                try:
                    os.close(descriptor)
                except OSError as error:
                    if error.errno != errno.EBADF:
                        raise
            pidfd_patch.stop()

    def test_observer_deadline_expiry_after_pidfd_acquisition_closes_local_handles(self) -> None:
        ready, release = self.work / "deadline-ready", self.work / "deadline-release"
        ready.write_text("123 456\n")
        opened: list[int] = []
        descriptors: list[int] = []
        thread: threading.Thread | None = None

        def open_fake_pidfd(number: int) -> int:
            opened.append(number)
            return os.open(os.devnull, os.O_RDONLY)

        signal_patch = mock.patch.object(signal, "pidfd_send_signal")
        signal_patch.start()
        self.addCleanup(signal_patch.stop)
        pidfd_patch = mock.patch(f"{__name__}.REAL_PIDFD_OPEN", side_effect=open_fake_pidfd)
        pidfd_patch.start()
        self.addCleanup(pidfd_patch.stop)
        try:
            with mock.patch.object(time, "monotonic", side_effect=[10.0, 10.5, 13.0]):
                descriptors, thread, failures = self.observe_tree(ready, release)
                thread.join(timeout=2)
            self.assertFalse(thread.is_alive(), "observer did not finish after deadline expiry")
            self.assertEqual(opened, [123, 456])
            self.assertEqual(descriptors, [])
            self.assertEqual(len(failures), 1)
            self.assertRegex(str(failures[0]), "readiness timeout")
            self.assertFalse(release.exists(), "observer released after its deadline expired")
        finally:
            if thread is not None:
                thread.join(timeout=3)
            for descriptor in opened:
                try:
                    os.close(descriptor)
                except OSError as error:
                    if error.errno != errno.EBADF:
                        raise

    def test_timeout_stops_original_leader_and_same_session_descendant(self) -> None:
        for operation in ("run", "signal_when_ready"):
            with self.subTest(operation=operation):
                ready = self.work / f"{operation}-tree-ready"
                release = self.work / f"{operation}-tree-release"
                descriptors, thread, failures = self.observe_tree(ready, release)
                command = self.tree_command(ready, release, exit_leader=False)
                with self.assertRaisesRegex(AssertionError, "timeout"):
                    if operation == "run":
                        self.runner.run(command, timeout=.5)
                    else:
                        self.runner.signal_when_ready(command, ready, (), timeout=.5)
                self.check_tree_stopped(descriptors, thread, failures)

    def test_descendant_retaining_stdout_has_bounded_cleanup(self) -> None:
        ready, release = self.work / "pipe-ready", self.work / "pipe-release"
        descriptors, thread, failures = self.observe_tree(ready, release)
        started = time.monotonic()
        with self.assertRaisesRegex(AssertionError, "orphaned process group"):
            self.runner.run(self.tree_command(ready, release, exit_leader=True), timeout=.5)
        self.assertLess(time.monotonic() - started, 5, "held output pipe blocked cleanup")
        self.check_tree_stopped(descriptors, thread, failures)

    def test_nondefault_sigchld_rejects_before_spawn(self) -> None:
        ready = self.work / "ignored-ready"
        def inherited_handler(number, frame):
            pass

        for operation, disposition in (
            (operation, disposition)
            for operation in ("run", "signal_when_ready")
            for disposition in (signal.SIG_IGN, inherited_handler)
        ):
            with self.subTest(operation=operation, disposition=disposition):
                with mock.patch.object(runtime_boundary.signal, "getsignal", return_value=disposition), \
                        mock.patch.object(runtime_boundary.subprocess, "Popen",
                                          side_effect=AssertionError("spawn before SIGCHLD check")) as spawn:
                    with self.assertRaisesRegex(OSError, "SIGCHLD"):
                        command = self.command("pass")
                        if operation == "run":
                            self.runner.run(command)
                        else:
                            self.runner.signal_when_ready(command, ready, ())
                    spawn.assert_not_called()

    def test_lost_wait_authority_forbids_numeric_group_operations(self) -> None:
        ready = self.work / "lost-ready"
        for operation in ("run", "signal_when_ready"):
            with self.subTest(operation=operation):
                with mock.patch.object(runtime_boundary.os, "waitid",
                                       side_effect=ChildProcessError(errno.ECHILD, "injected lost child")), \
                        mock.patch.object(runtime_boundary.os, "killpg",
                                          side_effect=AssertionError("numeric group operation after ECHILD")) as group:
                    with self.assertRaises(ChildProcessError):
                        command = self.command("import time; time.sleep(30)")
                        if operation == "run":
                            self.runner.run(command, timeout=.05)
                        else:
                            self.runner.signal_when_ready(command, ready, (), timeout=.05)
                    group.assert_not_called()

    def test_observed_pidfd_closes_when_release_write_fails(self) -> None:
        observer = REAL_POPEN(self.command("import time; time.sleep(30)"))
        observer_guard = REAL_PIDFD_OPEN(observer.pid)
        ready, release = self.work / "observer-ready", self.work / "release-directory"
        ready.write_text(f"{observer.pid}\n")
        release.mkdir()
        opened: list[int] = []

        def track(number: int) -> int:
            descriptor = REAL_PIDFD_OPEN(number)
            opened.append(descriptor)
            return descriptor

        try:
            with mock.patch.object(runtime_boundary.os, "pidfd_open", side_effect=track):
                with self.assertRaises(IsADirectoryError):
                    self.runner.signal_when_ready(
                        self.command("import time; time.sleep(30)"), ready, (),
                        observe_ready_pid=True, release=release, timeout=1,
                    )
            self.assertEqual(len(opened), 2)
            for descriptor in opened:
                with self.assertRaises(OSError, msg="runner leaked an acquired pidfd") as error:
                    os.fstat(descriptor)
                self.assertEqual(error.exception.errno, errno.EBADF)
        finally:
            signal.pidfd_send_signal(observer_guard, signal.SIGKILL)
            observer.wait(timeout=3)
            os.close(observer_guard)
            # Close baseline leaks so a failing regression cannot poison later
            # tests. No allocation occurs between the assertions and this pass.
            for descriptor in opened:
                try:
                    os.close(descriptor)
                except OSError as error:
                    if error.errno != errno.EBADF:
                        raise

    def test_observer_permission_failure_does_not_stop_direct_cleanup(self) -> None:
        ready = self.work / "permission-observer-ready"
        observer = REAL_POPEN(self.command(
            "import os, pathlib, sys, tempfile, time; "
            "p=pathlib.Path(sys.argv[1]); "
            "f=tempfile.NamedTemporaryFile(mode='w',encoding='ascii',dir=p.parent,delete=False); "
            "f.write(str(os.getpid())+'\\n'); f.close(); os.replace(f.name,p); time.sleep(30)",
            ready,
        ))
        observer_guard = REAL_PIDFD_OPEN(observer.pid)
        opened: dict[int, int] = {}
        attempts: list[tuple[int, int]] = []

        def track(number: int) -> int:
            descriptor = REAL_PIDFD_OPEN(number)
            opened[number] = descriptor
            return descriptor

        def deliver(descriptor: int, number: int) -> None:
            attempts.append((descriptor, number))
            if descriptor == opened.get(observer.pid) and number == signal.SIGKILL:
                raise PermissionError(errno.EPERM, "injected observer cleanup denied")
            REAL_PIDFD_SEND_SIGNAL(descriptor, number)

        try:
            deadline = time.monotonic() + 2
            expected_ready = f"{observer.pid}\n"
            while not ready.exists() or ready.read_text(encoding="ascii") != expected_ready:
                self.assertLess(time.monotonic(), deadline, "observer readiness timeout")
                time.sleep(.005)
            with mock.patch.object(runtime_boundary.os, "pidfd_open", side_effect=track), \
                    mock.patch.object(runtime_boundary.os, "waitid",
                                      side_effect=ChildProcessError(errno.ECHILD, "injected lost child")), \
                    mock.patch.object(runtime_boundary.os, "killpg",
                                      side_effect=AssertionError("numeric operation after ECHILD")) as group, \
                    mock.patch.object(runtime_boundary.signal, "pidfd_send_signal", side_effect=deliver):
                with self.assertRaises(Exception) as error:
                    self.runner.signal_when_ready(
                        self.command("import time; time.sleep(30)"), ready, (),
                        observe_ready_pid=True, timeout=1,
                    )
                group.assert_not_called()
            direct, direct_guard = self.owned.children[-1]
            with self.subTest(requirement="direct cleanup attempted"):
                self.assertIn((opened[direct.pid], signal.SIGKILL), attempts)
            with self.subTest(requirement="original direct child terminated"):
                self.assertTrue(select.select([direct_guard], [], [], .5)[0],
                                "observer refusal left the original direct child live")
            for descriptor in opened.values():
                with self.subTest(requirement="runner descriptor closed", descriptor=descriptor):
                    with self.assertRaises(OSError) as closed:
                        os.fstat(descriptor)
                    self.assertEqual(closed.exception.errno, errno.EBADF)
            diagnostic = "".join(traceback.format_exception(error.exception))
            with self.subTest(requirement="primary wait failure preserved"):
                self.assertIsInstance(error.exception, ChildProcessError)
            self.assertIn("injected lost child", diagnostic)
            self.assertIn("injected observer cleanup denied", diagnostic)
        finally:
            REAL_PIDFD_SEND_SIGNAL(observer_guard, signal.SIGKILL)
            observer.wait(timeout=3)
            os.close(observer_guard)

    def test_adapter_pidfd_refusal_cleans_registered_child_and_streams(self) -> None:
        adapter = OwnedProcesses()
        original_spawn, original_open = REAL_POPEN, REAL_PIDFD_OPEN
        children: list[subprocess.Popen] = []
        guards: list[int] = []

        def remember_spawn(*args, **kwargs) -> subprocess.Popen:
            process = original_spawn(*args, **kwargs)
            children.append(process)
            return process

        def refuse(number: int) -> int:
            # Keep an independent identity before refusing the adapter's own
            # acquisition, so even the failing baseline is safely recoverable.
            guards.append(original_open(number))
            raise OSError(errno.EMFILE, "injected adapter pidfd exhaustion")

        try:
            with mock.patch(f"{__name__}.REAL_POPEN", side_effect=remember_spawn), \
                    mock.patch(f"{__name__}.REAL_PIDFD_OPEN", side_effect=refuse):
                with self.assertRaises(OSError) as error:
                    adapter.spawn(
                        self.command("import time; time.sleep(30)"),
                        start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    )
                self.assertEqual(error.exception.errno, errno.EMFILE)
            self.assertEqual(len(children), 1)
            with self.subTest(requirement="unhandled adapter child stopped"):
                self.assertTrue(select.select(guards, [], [], .5)[0],
                                "pidfd refusal left an untracked direct child live")
            for name in ("stdout", "stderr"):
                with self.subTest(requirement="adapter output stream closed", stream=name):
                    self.assertTrue(getattr(children[0], name).closed)
        finally:
            for descriptor in guards:
                try:
                    REAL_PIDFD_SEND_SIGNAL(descriptor, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                finally:
                    os.close(descriptor)
            for process in children:
                process.wait(timeout=3)
                for stream in (process.stdout, process.stderr):
                    stream.close()
            adapter.close()

    def test_adapter_acquisition_lost_authority_forbids_numeric_cleanup(self) -> None:
        adapter = OwnedProcesses()
        original_spawn, original_open = REAL_POPEN, REAL_PIDFD_OPEN
        children: list[subprocess.Popen] = []
        guards: list[int] = []

        def remember_spawn(*args, **kwargs) -> subprocess.Popen:
            process = original_spawn(*args, **kwargs)
            children.append(process)
            return process

        def refuse(number: int) -> int:
            guards.append(original_open(number))
            raise OSError(errno.EMFILE, "injected adapter pidfd exhaustion")

        try:
            with mock.patch(f"{__name__}.REAL_POPEN", side_effect=remember_spawn), \
                    mock.patch(f"{__name__}.REAL_PIDFD_OPEN", side_effect=refuse), \
                    mock.patch(f"{__name__}.REAL_WAITID", side_effect=ChildProcessError(errno.ECHILD, "lost adapter child")), \
                    mock.patch(f"{__name__}.REAL_KILLPG", side_effect=AssertionError("numeric cleanup after ECHILD")) as group:
                with self.assertRaises(ChildProcessError):
                    adapter.spawn(
                        self.command("import time; time.sleep(30)"), start_new_session=True,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    )
                group.assert_not_called()
            self.assertFalse(select.select(guards, [], [], 0)[0])
            for stream in (children[0].stdout, children[0].stderr):
                self.assertTrue(stream.closed)
        finally:
            for descriptor in guards:
                try:
                    REAL_PIDFD_SEND_SIGNAL(descriptor, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                finally:
                    os.close(descriptor)
            for process in children:
                process.wait(timeout=3)
                for stream in (process.stdout, process.stderr):
                    stream.close()
            adapter.close()

    def test_adapter_cleanup_refusal_does_not_skip_other_child_or_handles(self) -> None:
        adapter = OwnedProcesses()
        guards: list[int] = []
        children: list[subprocess.Popen] = []
        try:
            for _ in range(2):
                process = adapter.spawn(
                    self.command("import time; time.sleep(30)"), start_new_session=True,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                )
                children.append(process)
                guards.append(REAL_PIDFD_OPEN(process.pid))
            refused: list[int] = []
            original_deliver = REAL_PIDFD_SEND_SIGNAL

            def deliver(descriptor: int, number: int) -> None:
                if not refused and number == signal.SIGKILL:
                    refused.append(descriptor)
                    raise PermissionError(errno.EPERM, "injected adapter cleanup refusal")
                original_deliver(descriptor, number)

            with mock.patch(f"{__name__}.REAL_PIDFD_SEND_SIGNAL", side_effect=deliver), \
                    mock.patch.object(runtime_boundary.signal, "pidfd_send_signal", side_effect=deliver):
                with self.assertRaises(Exception) as error:
                    adapter.close()
            with self.subTest(requirement="cleanup permission failure preserved"):
                self.assertIsInstance(error.exception, PermissionError)
            self.assertEqual(len(refused), 1)
            refused_index = next(index for index, (_, descriptor) in enumerate(adapter.children)
                                 if descriptor == refused[0])
            with self.subTest(requirement="other original child terminated"):
                self.assertTrue(select.select([guards[1 - refused_index]], [], [], .5)[0],
                                "one refused cleanup skipped the other original child")
            self.assertFalse(select.select([guards[refused_index]], [], [], 0)[0])
            for process, descriptor in adapter.children:
                with self.subTest(requirement="adapter descriptor closed", descriptor=descriptor):
                    with self.assertRaises(OSError) as closed:
                        os.fstat(descriptor)
                    self.assertEqual(closed.exception.errno, errno.EBADF)
                for stream in (process.stdout, process.stderr):
                    with self.subTest(requirement="adapter stream closed", descriptor=descriptor):
                        self.assertTrue(stream.closed)
        finally:
            for descriptor in guards:
                try:
                    REAL_PIDFD_SEND_SIGNAL(descriptor, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                finally:
                    os.close(descriptor)
            for process in children:
                process.wait(timeout=3)
                for stream in (process.stdout, process.stderr):
                    stream.close()
            adapter.close()


if __name__ == "__main__":
    main()
