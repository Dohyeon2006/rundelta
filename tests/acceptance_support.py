"""Shared unittest exit policy; strict release checks cannot pass by skipping."""

import os
import signal
import subprocess
import tempfile
import time
from pathlib import Path
import shutil
import unittest


def resolve_strace(root: Path, environment=None) -> Path | None:
    """Choose one absolute backend for both capability checks and actual tests.

    An explicit invalid override fails closed. Relative overrides are relative
    to the repository root, never the temporary target working directory.
    """
    environment = os.environ if environment is None else environment
    configured = environment.get("RUNDELTA_STRACE")
    if configured is not None:
        candidate = Path(configured)
        if not candidate.is_absolute():
            if len(candidate.parts) == 1:
                found = shutil.which(configured, path=environment.get("PATH"))
                if found is None:
                    return None
                candidate = Path(found)
            else:
                candidate = root / candidate
    else:
        candidate = root / "target/tools/strace-6.16/src/strace"
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            found = shutil.which("strace", path=environment.get("PATH"))
            if found is None:
                return None
            candidate = Path(found)
    return candidate.resolve() if candidate.is_file() and os.access(candidate, os.X_OK) else None


def strace_environment(root: Path, environment=None) -> dict[str, str]:
    """Freeze the resolved backend into a test subprocess environment."""
    result = dict(os.environ if environment is None else environment)
    backend = resolve_strace(root, result)
    if backend is None:
        raise RuntimeError("an executable strace backend is unavailable")
    result["RUNDELTA_STRACE"] = str(backend)
    return result


def successful(result: unittest.TestResult, *, strict: bool) -> bool:
    """Retain developer behavior; require executed, non-skipped release tests."""
    return result.wasSuccessful() and (
        not strict or (result.testsRun > 0 and not result.skipped and not result.expectedFailures)
    )


def main(*, verbosity: int = 2) -> None:
    """Run the caller's __main__ tests, preserving unittest CLI and finally cleanup."""
    result = unittest.main(verbosity=verbosity, exit=False).result
    strict = os.environ.get("RUNDELTA_RELEASE_GATE") == "1"
    raise SystemExit(0 if successful(result, strict=strict) else 1)


def require_owned_group_context():
    """Reject detectable auto-reaping before starting a test-owned process.

    The harness also requires no competing reaper or native SA_NOCLDWAIT mutation
    during the child's lifetime; Python's handler API cannot prove those facts.
    """
    if signal.getsignal(signal.SIGCHLD) != signal.SIG_DFL:
        raise OSError("test-owned groups require the default SIGCHLD disposition")


def wait_owned_group(process: subprocess.Popen, timeout: float) -> int:
    """Bound and reap one private process group without racing recycled leader IDs.

    WNOWAIT retains the leader as our unreaped child until group cleanup completes.
    Descendants that deliberately create another session are outside this group
    contract; arbitrary external processes are never selected by name or proc scan.
    """
    deadline = time.monotonic() + timeout
    child_owned = True
    try:
        while True:
            exited = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if exited is not None:
                break
            if time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired(process.args, timeout)
            time.sleep(min(0.02, max(0.001, deadline - time.monotonic())))
    except ChildProcessError:
        # A violated reaper premise is an error, never permission to signal a
        # numeric group ID whose original leader we no longer own.
        child_owned = False
        raise
    finally:
        # The direct child has not been reaped, so its private group identifier
        # cannot be reused by an unrelated process before this kill.
        if child_owned:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
    return process.returncode



def emit_command_output(stdout, stderr):
    """Write captured bytes without discarding undecodable subprocess evidence."""
    import sys
    for stream, payload in ((sys.stdout, stdout), (sys.stderr, stderr)):
        if hasattr(stream, "buffer"):
            stream.buffer.write(payload)
            stream.buffer.flush()
        else:
            stream.write(payload.decode("utf-8", errors="replace"))


def run_owned_command(command, *, cwd=None, env=None, timeout=30, check=False,
                      text=False, capture_output=False):
    """Run a test-owned group with separate file-backed output and finite lifetime.

    File-backed capture avoids descendants retaining pipes and blocking cleanup.
    A timeout retains raw output on TimeoutExpired; non-capturing callers also
    receive it through their console/evidence tee before the failure propagates.
    This helper controls the private group, not descendants that create a session.
    """
    require_owned_group_context()
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        process = subprocess.Popen(command, cwd=cwd, env=env, start_new_session=True,
                                   stdout=output, stderr=errors)
        failure = None
        try:
            returncode = wait_owned_group(process, timeout)
        except subprocess.TimeoutExpired as error:
            failure = error
        output.seek(0); errors.seek(0)
        stdout, stderr = output.read(), errors.read()
    if not capture_output:
        emit_command_output(stdout, stderr)
    if failure is not None:
        failure.output, failure.stderr = stdout, stderr
        raise failure
    if text:
        stdout, stderr = stdout.decode(), stderr.decode()
    result = subprocess.CompletedProcess(command, returncode, stdout, stderr)
    if check:
        result.check_returncode()
    return result
