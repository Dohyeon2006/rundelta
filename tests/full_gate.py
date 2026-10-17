#!/usr/bin/env python3
"""Run the development release gate and report pass/fail/skip as JSON."""

from __future__ import annotations

import argparse
import contextlib
import errno
import hashlib
import importlib.util
import io
import json
import math
import os
from pathlib import Path, PurePosixPath
import platform
import re
import signal
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import time
import unittest
from unittest import mock

from acceptance_support import emit_command_output, require_owned_group_context, resolve_strace, successful, wait_owned_group, run_owned_command


ROOT = Path(__file__).resolve().parents[1]
ACCEPTANCE_CHECKS = (
    "completion_equivalence_regressions", "cwd_uncertainty_regressions",
    "fs_context_regressions", "mount_namespace_regressions", "real_capture",
    "reliability_regressions", "runtime_boundary_regressions", "storage_cli_regressions",
)
REQUIRED_CHECKS = frozenset({
    "format", "clippy", "unit_and_default_tests", "public_archive_freeze",
    "build", "rustdoc", "capture_runner_oracles", "namespace_unshare_regressions", "diff_check", "package_allowlist", "acceptance_inventory", "audit_oracles", *ACCEPTANCE_CHECKS,
})
AUDIT_ORACLE_MODULES = ("test_fresh_field_audit", "test_field_warning_contract", "test_cwd_tree_warning_contract", "test_public_archive_freeze", "test_release_closure", "test_export_safety")
# Cargo gives encoded flags priority over RUSTDOCFLAGS; pin both forms so an
# inherited environment cannot weaken the documented strict check.
STRICT_RUSTDOC_ENV = {"RUSTDOCFLAGS": "-D warnings", "CARGO_ENCODED_RUSTDOCFLAGS": "-D\x1fwarnings"}


def validate_oracle_inventory(names):
    require(len(names) == len(AUDIT_ORACLE_MODULES) and set(names) == set(AUDIT_ORACLE_MODULES),
            "oracle modules differ from the unique required inventory")


def audit_oracles() -> int:
    """Run every registered oracle module; empty, skipped or broken suites fail."""
    validate_oracle_inventory([path.stem for path in (ROOT / "tests").glob("test_*.py")])
    return strict_oracles(AUDIT_ORACLE_MODULES)


def strict_oracles(names) -> int:
    """Execute explicit safety suites without treating empty or skipped tests as pass."""
    loader = unittest.TestLoader()
    suite = unittest.TestSuite()
    for name in names:
        module = loader.loadTestsFromName(name)
        require(module.countTestCases() > 0, f"empty oracle suite: {name}")
        suite.addTest(module)
    require(not loader.errors, "oracle test import/registration failed: " + "; ".join(loader.errors))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if successful(result, strict=True) else 1

# Compilation and package builds have a larger budget than subprocess acceptance.
# Explicit overrides support slower machines; neither a stuck group nor the whole
# gate is permitted to wait forever.
DEFAULT_CHECK_TIMEOUT = 600.0
DEFAULT_GATE_TIMEOUT = 3600.0


class EvidenceTee:
    """Preserve raw subprocess diagnostics while keeping ordinary console output."""

    def __init__(self, console, evidence):
        self.console = console
        self.evidence = evidence

    @property
    def buffer(self):
        return self

    def write(self, value):
        payload = value.encode("utf-8") if isinstance(value, str) else value
        self.evidence.write(payload)
        if isinstance(value, bytes) and hasattr(self.console, "buffer"):
            return self.console.buffer.write(value)
        return self.console.write(value if isinstance(value, str) else value.decode("utf-8", errors="replace"))

    def flush(self):
        self.evidence.flush()
        self.console.flush()


@contextlib.contextmanager
def check_log(directory, name):
    """Attach raw logs to composite checks that run several owned commands."""
    if directory is None:
        yield
        return
    with (directory / (name + ".log")).open("wb") as stream:
        with contextlib.redirect_stdout(EvidenceTee(sys.stdout, stream)), contextlib.redirect_stderr(EvidenceTee(sys.stderr, stream)):
            yield


def attach_check_log(summary, directory, name):
    if directory is not None:
        path = directory / (name + ".log")
        with path.open("rb") as stream:
            summary[-1].update(log=path.name, log_sha256=hashlib.file_digest(stream, "sha256").hexdigest())


def available(command: str) -> bool:
    return shutil.which(command) is not None


def gate_environment() -> dict[str, str]:
    """Return a deterministic environment that cannot disable Python assertions."""
    environment = os.environ.copy()
    environment.pop("PYTHONOPTIMIZE", None)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return environment


def checked_output(command, *, cwd=None, env=None, text=False):
    """Bound auxiliary Git commands as well as the long-running product checks."""
    return run_owned_command(command, cwd=cwd, env=env, text=text,
                             capture_output=True, check=True, timeout=30).stdout


def executable(value: str | Path) -> bool:
    spelling = os.fspath(value)
    candidate = Path(spelling)
    if candidate.is_absolute() or len(candidate.parts) > 1:
        if not candidate.is_absolute():
            candidate = ROOT / candidate
        return candidate.is_file() and os.access(candidate, os.X_OK)
    return available(spelling)


def has_pidfd() -> bool:
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        return False
    descriptor = None
    try:
        descriptor = os.pidfd_open(os.getpid())
        signal.pidfd_send_signal(descriptor, 0)
    except OSError:
        return False
    finally:
        if descriptor is not None:
            os.close(descriptor)
    return True


def strace_available() -> bool:
    return resolve_strace(ROOT) is not None


def has_pty() -> bool:
    try:
        master, slave = os.openpty()
    except (AttributeError, OSError):
        return False
    os.close(master)
    os.close(slave)
    return True


def has_dev_full() -> bool:
    try:
        with open("/dev/full", "wb", buffering=0) as sink:
            try:
                sink.write(b"x")
            except OSError as error:
                return error.errno == errno.ENOSPC
    except OSError:
        return False
    return False


def mount_namespace_capability_reason() -> str | None:
    missing = [
        command
        for command in ("unshare", "mount", "umount")
        if not available(command)
    ]
    if missing:
        return "required mount-namespace tools are unavailable: " + ", ".join(missing)
    try:
        with tempfile.TemporaryDirectory(prefix="rundelta-mount-capability-") as directory:
            completed = run_owned_command(
                [
                    "unshare",
                    "--user",
                    "--map-root-user",
                    "--mount",
                    "/bin/sh",
                    "-c",
                    'mount -t tmpfs none "$1" && umount "$1"',
                    "mount-capability",
                    directory,
                ],
                cwd=ROOT,
                env=gate_environment(),
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
    except (OSError, subprocess.TimeoutExpired) as error:
        return f"user+mount namespace capability probe could not complete: {error}"
    if completed.returncode == 0:
        return None
    detail = completed.stderr.strip() or completed.stdout.strip()
    return "user+mount namespace or tmpfs mount unavailable: " + (
        detail or f"exit {completed.returncode}"
    )


def append_result(
    summary: list[dict[str, object]],
    name: str,
    status: str,
    *,
    command: list[str] | None = None,
    reason: str | None = None,
    returncode: int | None = None,
    stage: str | None = None,
) -> None:
    result: dict[str, object] = {"name": name, "status": status}
    if command is not None:
        result["command"] = command
    if reason is not None:
        result["reason"] = reason
    if returncode is not None:
        result["returncode"] = returncode
    if stage is not None:
        result["stage"] = stage
    summary.append(result)


def skip_check(
    summary: list[dict[str, object]],
    name: str,
    reason: str,
    command: list[str] | None = None,
) -> None:
    append_result(summary, name, "skipped", command=command, reason=reason)
    print(f"SKIP  {name}: {reason}", flush=True)


def fail_check(
    summary: list[dict[str, object]],
    name: str,
    reason: str,
    *,
    command: list[str] | None = None,
    returncode: int | None = None,
    stage: str | None = None,
) -> None:
    append_result(
        summary,
        name,
        "failed",
        command=command,
        reason=reason,
        returncode=returncode,
        stage=stage,
    )
    print(f"FAILED {name}: {reason}", flush=True)


def run_check(
    summary: list[dict[str, object]],
    name: str,
    command: list[str],
    reason: str | None = None,
    *,
    cwd: Path | None = None,
    timeout: float = DEFAULT_CHECK_TIMEOUT,
    evidence_dir: Path | None = None,
    environment: dict[str, str] | None = None,
) -> None:
    if reason is not None:
        skip_check(summary, name, reason, command)
        return
    print(f"RUN   {name}: {shlex.join(command)}", flush=True)
    started = time.monotonic()
    try:
        if timeout <= 0:
            raise subprocess.TimeoutExpired(command, timeout)
        require_owned_group_context()
        with tempfile.TemporaryFile() if evidence_dir is None else (evidence_dir / (name + ".log")).open("w+b") as output:
            process = subprocess.Popen(command, cwd=ROOT if cwd is None else cwd,
                                       env=gate_environment() | (environment or {}), start_new_session=True,
                                       stdout=output, stderr=subprocess.STDOUT)
            returncode = wait_owned_group(process, timeout)
            output.seek(0)
            digest = hashlib.sha256()
            while chunk := output.read(64 * 1024):
                digest.update(chunk)
                if hasattr(sys.stdout, "buffer"):
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
                else:
                    sys.stdout.write(chunk.decode("utf-8", errors="replace"))
    except subprocess.TimeoutExpired:
        fail_check(summary, name, f"check deadline exceeded ({timeout:g} seconds)",
                   command=command, stage="deadline")
        summary[-1].update(timeout_seconds=timeout, elapsed_seconds=time.monotonic() - started)
        if evidence_dir is not None:
            path = evidence_dir / (name + ".log")
            if path.exists():
                with path.open("rb") as saved:
                    summary[-1].update(log=path.name, log_sha256=hashlib.file_digest(saved, "sha256").hexdigest())
        return
    except OSError as error:
        fail_check(
            summary,
            name,
            f"could not start command: {error}",
            command=command,
            stage="spawn",
        )
        return
    status = "passed" if returncode == 0 else "failed"
    append_result(
        summary,
        name,
        status,
        command=command,
        returncode=returncode,
    )
    summary[-1].update(timeout_seconds=timeout, elapsed_seconds=time.monotonic() - started,
                       log_sha256=digest.hexdigest())
    if environment:
        summary[-1]["environment"] = environment
    if evidence_dir is not None:
        summary[-1]["log"] = name + ".log"
    print(f"{status.upper():6} {name}", flush=True)



def acceptance_reason(name: str) -> str | None:
    if platform.system() != "Linux":
        return "Linux is required"
    if not available("python3"):
        return "python3 is unavailable"
    if not has_pidfd():
        return "pidfd_open is unavailable"
    if name == "completion_equivalence_regressions":
        return None
    if not strace_available():
        return "an executable strace backend is unavailable"
    if name == "mount_namespace_regressions":
        return mount_namespace_capability_reason()
    if name == "storage_cli_regressions":
        if not available("cc"):
            return "a C compiler is unavailable"
        if os.geteuid() == 0:
            return "the permission regression requires a non-root user"
        if not has_dev_full():
            return "a writable /dev/full with ENOSPC semantics is unavailable"
        return None
    if not available("cc"):
        return "a C compiler is unavailable"
    if name in {
        "cwd_uncertainty_regressions",
        "fs_context_regressions",
        "reliability_regressions",
    } and platform.machine().lower() not in {"x86_64", "amd64"}:
        return "Linux x86_64 is required"
    if name == "reliability_regressions" and not has_pty():
        return "a usable PTY is unavailable"
    return None


def capture_runner_reason() -> str | None:
    """The runner suite needs stable Linux child identities, without strace."""
    if platform.system() != "Linux":
        return "Linux is required"
    if not available("python3"):
        return "python3 is unavailable"
    if not has_pidfd():
        return "pidfd_open is unavailable"
    if not all(hasattr(os, name) for name in ("waitid", "P_PID", "WEXITED", "WNOHANG", "WNOWAIT")):
        return "waitid with WNOWAIT is unavailable"
    return None


def namespace_unshare_reason() -> str | None:
    """Probe the stronger user/network namespace requirement separately from FS."""
    reason = acceptance_reason("namespace_unshare_regressions")
    if reason is not None:
        return reason
    if not available("unshare"):
        return "util-linux unshare is unavailable"
    try:
        completed = run_owned_command(
            ["unshare", "--user", "--map-root-user", "unshare", "--net", "/bin/true"],
            cwd=ROOT, env=gate_environment(), capture_output=True, text=True,
            timeout=30, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return f"user/network namespace capability probe could not complete: {error}"
    if completed.returncode == 0:
        return None
    return "user/network namespace unavailable: " + (completed.stderr.strip() or
            completed.stdout.strip() or f"exit {completed.returncode}")


def extract_clean_tree(data: bytes, destination: Path) -> None:
    """Extract a git archive without accepting links or path traversal."""
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as archive:
        for member in archive.getmembers():
            path = PurePosixPath(member.name)
            if path.is_absolute() or not path.parts or ".." in path.parts:
                raise ValueError(f"unsafe git archive member: {member.name!r}")
            target = destination.joinpath(*path.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile():
                raise ValueError(f"unsupported git archive member: {member.name!r}")
            target.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is None:
                raise ValueError(f"unreadable git archive member: {member.name!r}")
            with target.open("wb") as output:
                shutil.copyfileobj(source, output)
            target.chmod(member.mode & 0o777)


def package_archive(target: Path) -> Path:
    archives = sorted(
        path for path in (target / "package").glob("*.crate") if path.is_file()
    )
    if len(archives) != 1:
        raise ValueError(f"expected one package archive, found {len(archives)}")
    return archives[0]


def clean_revision_archive(summary: list[dict[str, object]]) -> bytes | None:
    name = "package_allowlist"
    for command in ("git", "cargo"):
        if not available(command):
            fail_check(
                summary,
                name,
                f"required command is unavailable: {command}",
                stage="prerequisite",
            )
            return
    if sys.version_info < (3, 11):
        fail_check(
            summary,
            name,
            "Python 3.11 or newer is required by the package checker",
            stage="prerequisite",
        )
        return

    status_command = ["git", "status", "--porcelain=v1", "--untracked-files=all"]
    try:
        status = run_owned_command(
            status_command,
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        fail_check(
            summary,
            name,
            f"could not inspect the working tree: {error}",
            command=status_command,
            stage="working_tree",
        )
        return
    if status.returncode != 0:
        fail_check(
            summary,
            name,
            "git status failed",
            command=status_command,
            returncode=status.returncode,
            stage="working_tree",
        )
        return
    if status.stdout:
        fail_check(
            summary,
            name,
            "the working tree is dirty; package verification must describe the same clean revision as the other gates",
            command=status_command,
            stage="working_tree",
        )
        return

    archive_command = ["git", "archive", "--format=tar", "HEAD"]
    try:
        exported = run_owned_command(
            archive_command,
            cwd=ROOT,
            capture_output=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        fail_check(
            summary,
            name,
            f"could not export the clean revision: {error}",
            command=archive_command,
            stage="clean_export",
        )
        return
    if exported.returncode != 0:
        fail_check(
            summary,
            name,
            "git archive failed",
            command=archive_command,
            returncode=exported.returncode,
            stage="clean_export",
        )
        return

    return exported.stdout


def package_gate(
    summary: list[dict[str, object]], exported: bytes | None = None,
    *, repository: Path | None = None,
    timeout: float = DEFAULT_CHECK_TIMEOUT,
) -> None:
    """Check one immutable archive, or require a clean HEAD in ordinary mode."""
    name = "package_allowlist"
    started = time.monotonic()
    if exported is None:
        exported = clean_revision_archive(summary)
        if exported is None:
            return

    with tempfile.TemporaryDirectory(prefix="rundelta-full-gate-") as directory:
        work = Path(directory)
        source = work / "source"
        target = work / "target"
        source.mkdir()
        try:
            extract_clean_tree(exported, source)
        except (OSError, tarfile.TarError, ValueError) as error:
            fail_check(
                summary,
                name,
                f"clean revision extraction failed: {error}",
                stage="clean_export",
            )
            return

        environment = gate_environment() | {"CARGO_TARGET_DIR": str(target)}
        cargo_command = ["cargo", "package", "--locked", "--offline"]
        print(f"RUN   {name}: {shlex.join(cargo_command)} (isolated clean tree)", flush=True)
        try:
            packaged = run_owned_command(
                cargo_command,
                cwd=source,
                env=environment,
                timeout=max(0.001, timeout - (time.monotonic() - started)),
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            fail_check(
                summary,
                name,
                f"could not start cargo package: {error}",
                command=cargo_command,
                stage="cargo_package",
            )
            return
        if packaged.returncode != 0:
            fail_check(
                summary,
                name,
                "cargo package failed in the isolated clean tree",
                command=cargo_command,
                returncode=packaged.returncode,
                stage="cargo_package",
            )
            return
        try:
            crate = package_archive(target)
        except ValueError as error:
            fail_check(
                summary,
                name,
                str(error),
                command=cargo_command,
                stage="archive_location",
            )
            return

        checker = source / "docs/packaging/check_package.py"
        check_command = [
            sys.executable,
            "-I",
            "-B",
            str(checker),
            str(crate),
            "--source",
            str(source),
            "--private-prefix",
            str(ROOT),
            "--private-prefix",
            str(repository or ROOT),
            "--private-prefix",
            str(work),
            "--self-test",
        ]
        print(f"RUN   {name}: {shlex.join(check_command)}", flush=True)
        try:
            checked = run_owned_command(
                check_command,
                cwd=source,
                env=environment,
                timeout=max(0.001, timeout - (time.monotonic() - started)),
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            fail_check(
                summary,
                name,
                f"could not start the package privacy checker: {error}",
                command=check_command,
                stage="privacy_check",
            )
            return
        if checked.returncode != 0:
            fail_check(
                summary,
                name,
                "package allowlist/privacy verification failed",
                command=check_command,
                returncode=checked.returncode,
                stage="privacy_check",
            )
            return
    append_result(
        summary,
        name,
        "passed",
        command=check_command,
        returncode=0,
        stage="privacy_check",
    )
    print(f"PASSED {name}", flush=True)


def counts(summary: list[dict[str, object]]) -> dict[str, int]:
    return {
        state: sum(check["status"] == state for check in summary)
        for state in ("passed", "failed", "skipped")
    }


def exit_code(summary: list[dict[str, object]]) -> int:
    return 1 if counts(summary)["failed"] else 0


def release_errors(summary: list[dict[str, object]], *, candidate: bool) -> list[str]:
    """Require the complete, unique gate inventory, not just zero failed checks."""
    required = REQUIRED_CHECKS | ({"release_closure", "candidate_snapshot"} if candidate else set())
    names = [check["name"] for check in summary]
    errors = [f"missing required check: {name}" for name in sorted(required - set(names))]
    for name in sorted(set(names)):
        if names.count(name) != 1:
            errors.append(f"duplicate check: {name}")
    for check in summary:
        if check["status"] != "passed":
            errors.append(f"required check did not pass: {check['name']} ({check['status']})")
    return errors


def validate_acceptance_inventory(output: str) -> None:
    """Exact cargo test filters can silently match zero tests; check the built list."""
    names = [line.removesuffix(": test") for line in output.splitlines() if line.endswith(": test")]
    require(len(names) == len(ACCEPTANCE_CHECKS) and set(names) == set(ACCEPTANCE_CHECKS),
            "compiled ignored acceptance tests differ from the required gate inventory")


def acceptance_inventory(summary: list[dict[str, object]], *, timeout: float = DEFAULT_CHECK_TIMEOUT) -> None:
    command = ["cargo", "test", "--test", "acceptance", "--", "--list", "--ignored"]
    try:
        listed = run_owned_command(command, cwd=ROOT, env=gate_environment(),
                                   capture_output=True, timeout=timeout)
        # Preserve the actual inventory output, including invalid UTF-8 or a
        # failed cargo invocation, before applying the text registration check.
        emit_command_output(listed.stdout, listed.stderr)
        if listed.returncode:
            raise subprocess.CalledProcessError(listed.returncode, command, listed.stdout, listed.stderr)
        validate_acceptance_inventory(listed.stdout.decode("utf-8"))
    except subprocess.TimeoutExpired as error:
        emit_command_output(error.output or b"", error.stderr or b"")
        fail_check(summary, "acceptance_inventory", str(error), command=command, stage="deadline")
    except (OSError, UnicodeError, subprocess.CalledProcessError, AssertionError) as error:
        fail_check(summary, "acceptance_inventory", str(error), command=command)
    else:
        append_result(summary, "acceptance_inventory", "passed", command=command, returncode=0)
        print("PASSED acceptance_inventory", flush=True)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def self_test() -> int:
    global ROOT
    summary: list[dict[str, object]] = []
    with contextlib.redirect_stdout(io.StringIO()):
        run_check(
            summary,
            "synthetic_pass",
            [sys.executable, "-c", "raise SystemExit(0)"],
        )
        run_check(
            summary,
            "synthetic_fail",
            [sys.executable, "-c", "raise SystemExit(7)"],
        )
        run_check(summary, "synthetic_skip", [], "synthetic unavailable capability")
    require(
        [entry["status"] for entry in summary] == ["passed", "failed", "skipped"],
        "pass/fail/skip statuses were not kept distinct",
    )
    require(
        counts(summary) == {"passed": 1, "failed": 1, "skipped": 1},
        "status totals are incorrect",
    )
    require(exit_code(summary) == 1, "a failed check did not make the gate fail")
    with mock.patch.dict(os.environ, {"RUSTDOCFLAGS": "-A warnings", "CARGO_ENCODED_RUSTDOCFLAGS": "-Awarnings"}), contextlib.redirect_stdout(io.StringIO()):
        strict = []
        run_check(strict, "strict_rustdoc_environment", [sys.executable, "-c",
                  "import os; assert os.environ['RUSTDOCFLAGS'] == '-D warnings'; "
                  "assert os.environ['CARGO_ENCODED_RUSTDOCFLAGS'] == '-D\\x1fwarnings'"],
                  environment=STRICT_RUSTDOC_ENV)
    require(strict[0]["status"] == "passed" and strict[0]["environment"] == STRICT_RUSTDOC_ENV,
            "strict Rustdoc flags were weakened or omitted from saved command facts")
    with tempfile.TemporaryDirectory(prefix="gate-owned-group-control-") as directory:
        child_file = Path(directory) / "child.pid"
        child = ("import os,time,pathlib,tempfile; p=pathlib.Path(" + repr(str(child_file)) + "); "
                 "f=tempfile.NamedTemporaryFile(mode='w',dir=p.parent,prefix='.'+p.name+'.',suffix='.tmp',delete=False); "
                 "f.write(str(os.getpid())+'\\n'); f.close(); os.replace(f.name,p); time.sleep(60)")
        parent = "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c'," + repr(child) + "]); time.sleep(60)"
        deadline_summary = []
        started = time.monotonic()
        with contextlib.redirect_stdout(io.StringIO()):
            run_check(deadline_summary, "owned_hang", [sys.executable, "-c", parent], timeout=0.5)
        require(time.monotonic() - started < 5, "hung check was not bounded")
        require(deadline_summary[0]["status"] == "failed" and deadline_summary[0]["stage"] == "deadline",
                "timeout did not produce a traceable failed result")
        require(child_file.is_file(), "owned child control never started")
        child_marker = child_file.read_text()
        require(child_marker.endswith("\n") and child_marker.count("\n") == 1
                and child_marker[:-1].isascii() and child_marker[:-1].isdecimal()
                and int(child_marker[:-1]) > 0, "owned child marker was incomplete")
        child_pid = int(child_marker[:-1])
        stat = Path(f"/proc/{child_pid}/stat")
        require(not stat.exists() or stat.read_text().split(") ", 1)[1].startswith("Z"),
                "timeout left a live test-owned child")

    with tempfile.TemporaryDirectory(prefix="gate-backend-control-") as directory:
        root = Path(directory)
        system = root / "bin/strace"
        system.parent.mkdir(); system.write_text("#!/bin/sh\nexit 0\n"); system.chmod(0o755)
        environment = {"PATH": str(system.parent)}
        require(resolve_strace(root, environment) == system.resolve(), "system backend was not resolved")
        bundled = root / "target/tools/strace-6.16/src/strace"
        bundled.parent.mkdir(parents=True); bundled.write_bytes(system.read_bytes()); bundled.chmod(0o755)
        require(resolve_strace(root, environment) == bundled.resolve(), "bundled backend priority differs")
        require(resolve_strace(root, environment | {"RUNDELTA_STRACE": str(system)}) == system.resolve(),
                "explicit backend was not preserved")
        require(resolve_strace(root, environment | {"RUNDELTA_STRACE": "./absent"}) is None,
                "invalid explicit backend fell back")
        system.unlink(); bundled.unlink()
        require(resolve_strace(root, environment) is None, "absent backend was invented")

    # Actual Git archive/checkout controls with only product commands substituted.
    # These prove source binding and mutation rejection, not product acceptance.
    repository_before = ROOT
    with tempfile.TemporaryDirectory(prefix="gate-source-binding-control-") as directory:
        repository = Path(directory)
        def git(*args):
            return checked_output(["git", *args], cwd=repository).decode().strip()
        git("init", "-q"); git("config", "user.name", "Gate Control")
        git("config", "user.email", "gate-control@example.invalid")
        (repository / "input.txt").write_text("A\n"); git("add", "input.txt"); git("commit", "-qm", "A")
        base = git("rev-parse", "HEAD")
        (repository / "input.txt").write_text("B\n"); git("commit", "-qam", "B")
        other = git("rev-parse", "HEAD"); git("checkout", "-q", base)
        seen = []
        def change_head(*args, **kwargs):
            seen.append((ROOT / "input.txt").read_text())
            git("checkout", "-q", other)
            seen.append((ROOT / "input.txt").read_text())
        try:
            ROOT = repository
            with mock.patch.object(sys.modules[__name__], "run_gates", change_head), contextlib.redirect_stdout(io.StringIO()):
                try:
                    clean_snapshot_gate(False, [])
                except AssertionError as error:
                    require("HEAD changed" in str(error), "head mutation failed for an unrelated reason")
                else:
                    raise AssertionError("clean A-to-B checkout during validation was accepted")
            require(seen == ["A\n", "A\n"], "checks switched away from the fixed archive")
            git("checkout", "-q", base)
            def mutate_source(*args, **kwargs):
                (ROOT / "input.txt").write_text("mutated\n")
            with mock.patch.object(sys.modules[__name__], "run_gates", mutate_source), contextlib.redirect_stdout(io.StringIO()):
                try:
                    clean_snapshot_gate(False, [])
                except AssertionError as error:
                    require("bytes changed" in str(error), "source mutation failed for an unrelated reason")
                else:
                    raise AssertionError("test-mutated snapshot was accepted")
            def add_source(*args, **kwargs):
                (ROOT / "build.rs").write_text("fn main() {}\n")
            with mock.patch.object(sys.modules[__name__], "run_gates", add_source), contextlib.redirect_stdout(io.StringIO()):
                try:
                    clean_snapshot_gate(False, [])
                except AssertionError as error:
                    require("unexpected candidate file" in str(error), "added source failed for an unrelated reason")
                else:
                    raise AssertionError("a test-added build script was accepted")
            with mock.patch.object(sys.modules[__name__], "run_gates", lambda *args, **kwargs: None), contextlib.redirect_stdout(io.StringIO()):
                healthy = []
                clean_snapshot_gate(False, healthy)
            require(healthy[-1]["status"] == "passed", "fixed snapshot control failed")
        finally:
            ROOT = repository_before
    require(
        exit_code([{"name": "skip", "status": "skipped"}]) == 0,
        "a capability skip was treated as a failure",
    )
    complete = [{"name": name, "status": "passed"} for name in sorted(REQUIRED_CHECKS)]
    with mock.patch.object(unittest.TestLoader, "loadTestsFromName", return_value=unittest.TestSuite()), contextlib.redirect_stdout(io.StringIO()):
        try:
            strict_oracles(["synthetic_empty_runner"])
        except AssertionError as error:
            require("empty oracle suite" in str(error), "empty runner failed for unrelated reason")
        else:
            raise AssertionError("empty runner suite was accepted")
    validate_oracle_inventory(list(AUDIT_ORACLE_MODULES))
    for names in ([], list(AUDIT_ORACLE_MODULES[1:]), [*AUDIT_ORACLE_MODULES, "unregistered"], [*AUDIT_ORACLE_MODULES, AUDIT_ORACLE_MODULES[0]]):
        try:
            validate_oracle_inventory(names)
        except AssertionError:
            pass
        else:
            raise AssertionError("missing, extra or duplicated oracle module was accepted")
    require(not release_errors(complete, candidate=False), "complete release was rejected")
    require(bool(release_errors(complete, candidate=True)), "missing candidate closure was accepted")
    candidate = complete + [{"name": name, "status": "passed"}
                            for name in ("release_closure", "candidate_snapshot")]
    require(not release_errors(candidate, candidate=True), "complete candidate was rejected")
    for damaged in (
        [], complete[1:], complete + complete[:1],
        complete[:-1] + [{"name": complete[-1]["name"], "status": "skipped"}],
        complete[:-1] + [{"name": complete[-1]["name"], "status": "unknown"}],
    ):
        require(bool(release_errors(damaged, candidate=False)), "incomplete release was accepted")
    listing = "\n".join(name + ": test" for name in ACCEPTANCE_CHECKS)
    validate_acceptance_inventory(listing)
    for damaged in ("", listing.split("\n", 1)[1], listing + "\nextra: test"):
        try:
            validate_acceptance_inventory(damaged)
        except AssertionError:
            pass
        else:
            raise AssertionError("missing/extra acceptance registration was accepted")

    result = unittest.TestResult()
    require(successful(result, strict=False), "developer zero-test semantics changed")
    require(not successful(result, strict=True), "empty release unittest suite was accepted")
    result.testsRun = 1
    require(successful(result, strict=True), "passing release unittest suite was rejected")
    result.skipped.append(("fixture", "unavailable"))
    require(successful(result, strict=False), "developer skip semantics changed")
    require(not successful(result, strict=True), "nested unittest skip was accepted")
    # Check the actual subprocess exit as well as the pure decision function.
    child = (
        "import sys,unittest; sys.path.insert(0, 'tests'); "
        "from acceptance_support import main; "
        "exec('class Skipped(unittest.TestCase):\\n"
        " @unittest.skip(\"injected missing capability\")\\n"
        " def test_skip(self): pass'); main()"
    )
    for strict, expected in (("0", 0), ("1", 1)):
        completed = subprocess.run([sys.executable, "-B", "-c", child], cwd=ROOT,
                                   env=gate_environment() | {"RUNDELTA_RELEASE_GATE": strict},
                                   capture_output=True, text=True, check=False)
        require(completed.returncode == expected, "nested skip exit policy was not enforced")

    with tempfile.TemporaryDirectory(prefix="rundelta-full-gate-self-test-") as directory:
        work = Path(directory)
        valid = io.BytesIO()
        payload = b"fixture"
        with tarfile.open(fileobj=valid, mode="w:") as archive:
            member = tarfile.TarInfo("nested/file")
            member.size = len(payload)
            member.mode = 0o640
            archive.addfile(member, io.BytesIO(payload))
        destination = work / "valid"
        destination.mkdir()
        extract_clean_tree(valid.getvalue(), destination)
        require(
            (destination / "nested/file").read_bytes() == payload,
            "clean archive extraction changed file bytes",
        )
        verify_snapshot(valid.getvalue(), destination)
        copied = destination / "nested/file"
        for damage in ("bytes", "mode", "missing", "symlink"):
            if copied.exists() or copied.is_symlink():
                copied.unlink()
            copied.write_bytes(payload)
            copied.chmod(0o640)
            if damage == "bytes":
                copied.write_bytes(b"changed")
            elif damage == "mode":
                copied.chmod(0o600)
            else:
                copied.unlink()
                if damage == "symlink":
                    copied.symlink_to(work / "missing-target")
            try:
                verify_snapshot(valid.getvalue(), destination)
            except AssertionError:
                pass
            else:
                raise AssertionError(f"candidate {damage} mutation was accepted")

        unsafe = io.BytesIO()
        with tarfile.open(fileobj=unsafe, mode="w:") as archive:
            member = tarfile.TarInfo("../escape")
            member.size = len(payload)
            archive.addfile(member, io.BytesIO(payload))
        try:
            extract_clean_tree(unsafe.getvalue(), work / "unsafe")
        except ValueError:
            pass
        else:
            raise AssertionError("path-traversing archive was accepted")
        require(not (work / "escape").exists(), "archive extraction escaped its root")

        extraction_summary: list[dict[str, object]] = []
        with contextlib.redirect_stdout(io.StringIO()):
            package_gate(extraction_summary, b"invalid archive")
        require(
            len(extraction_summary) == 1
            and extraction_summary[0]["status"] == "failed"
            and extraction_summary[0]["stage"] == "clean_export",
            "package extraction error did not produce a structured failure",
        )

        target = work / "target"
        package = target / "package"
        package.mkdir(parents=True)
        try:
            package_archive(target)
        except ValueError:
            pass
        else:
            raise AssertionError("missing package archive was accepted")
        expected = package / "RunDelta-0.1.0.crate"
        expected.write_bytes(b"crate")
        require(package_archive(target) == expected, "package archive path is incorrect")
        (package / "unexpected.crate").write_bytes(b"crate")
        try:
            package_archive(target)
        except ValueError:
            pass
        else:
            raise AssertionError("ambiguous package archives were accepted")

    print("PASS: full-gate status, exit, archive-safety and package-location self-tests")
    evidence_failure_controls()
    package_optimization_controls()
    return 0


def evidence_failure_controls():
    """Exercise raw timeout logs, snapshot failure receipts and tool selection.

    Synthetic children and directory-selected tool shims test the harness only;
    actual product/toolchain validation still comes from the full snapshot gate.
    """
    global ROOT
    repository = ROOT
    with tempfile.TemporaryDirectory(prefix="gate-evidence-control-") as directory:
        work = Path(directory)
        child = "import os,time; os.write(1,b'OUT\\xff\\n'); os.write(2,b'ERR\\xfe\\n'); time.sleep(60)"
        for captured in (False, True):
            name = "captured_timeout" if captured else "tee_timeout"
            summary = []
            started = time.monotonic()
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                with check_log(work, name):
                    if captured:
                        actual_runner = run_owned_command
                        def inventory_child(command, **kwargs):
                            return actual_runner([sys.executable, "-c", child], **kwargs)
                        with mock.patch.object(sys.modules[__name__], "run_owned_command", inventory_child):
                            acceptance_inventory(summary, timeout=0.5)
                    else:
                        try:
                            run_owned_command([sys.executable, "-c", child], timeout=0.5)
                        except subprocess.TimeoutExpired as error:
                            require(error.output == b"OUT\xff\n" and error.stderr == b"ERR\xfe\n",
                                    "timeout discarded its raw captured output")
                            fail_check(summary, name, "deadline exceeded", stage="deadline")
                        else:
                            raise AssertionError("timeout control unexpectedly completed")
            raw = (work / (name + ".log")).read_bytes()
            require(time.monotonic() - started < 5 and summary[0]["status"] == "failed",
                    "composite timeout was not a bounded failure")
            require(b"OUT\xff\n" in raw and b"ERR\xfe\n" in raw,
                    "composite timeout lost pre-deadline stdout/stderr")

        index = work / "inert.index"
        index.touch()
        for candidate in (False, True):
            evidence = work / ("candidate_failure" if candidate else "clean_failure")
            argv = [str(Path(__file__)), "--release", "--evidence-dir", str(evidence)]
            if candidate:
                argv += ["--candidate-index", str(index)]
            with mock.patch.object(sys, "argv", argv), mock.patch.dict(os.environ), \
                    mock.patch.object(sys.modules[__name__], "checked_output",
                                      side_effect=subprocess.TimeoutExpired(["git", "rev-parse"], 30)), \
                    contextlib.redirect_stdout(io.StringIO()):
                require(main() == 1, "auxiliary timeout escaped the failed gate result")
            receipt = json.loads((evidence / "summary.json").read_text())
            name = "candidate_snapshot" if candidate else "source_snapshot"
            require(any(entry["name"] == name and entry["status"] == "failed"
                        for entry in receipt["checks"]), "snapshot timeout left no failure receipt")

        marker = work / "must-not-start"
        worker = work / "auto_reaper_control.py"
        worker.write_text(textwrap.dedent("""
            import contextlib, io, signal, sys
            from pathlib import Path
            sys.path.insert(0, sys.argv[1])
            from acceptance_support import run_owned_command
            from full_gate import run_check

            marker = Path(sys.argv[2])
            target = f"from pathlib import Path; Path({str(marker)!r}).write_text('started')"
            command = [sys.executable, '-c', target]
            signal.signal(signal.SIGCHLD, signal.SIG_IGN)
            try:
                run_owned_command(command)
            except OSError as error:
                assert 'SIGCHLD' in str(error)
            else:
                raise AssertionError('auto-reaper started an owned child')
            summary = []
            with contextlib.redirect_stdout(io.StringIO()):
                run_check(summary, 'auto_reaper', command)
            assert summary[0]['status'] == 'failed' and summary[0]['stage'] == 'spawn'
        """))
        run_owned_command([sys.executable, "-B", str(worker), str(ROOT / "tests"), str(marker)], cwd=ROOT, timeout=5,
                          capture_output=True, check=True)
        require(not marker.exists(), "inherited auto-reaper launched the target")
        process = mock.Mock(pid=999, args=["synthetic"])
        with mock.patch("acceptance_support.os.waitid", side_effect=ChildProcessError), \
                mock.patch("acceptance_support.os.killpg") as kill:
            try:
                wait_owned_group(process, 0.5)
            except ChildProcessError:
                pass
            else:
                raise AssertionError("lost child ownership was accepted")
            require(not kill.called and not process.wait.called,
                    "lost child ownership still selected a numeric group")

        bin_dir = work / "bin"
        bin_dir.mkdir()
        rustup = bin_dir / "rustup"
        rustup.write_text("#!" + sys.executable + "\nimport os,pathlib,sys\n"
                          "if sys.argv[1:2]==['which']:\n"
                          " print(pathlib.Path.cwd()/(sys.argv[2]+'.toolchain'))\n"
                          "else:\n os.write(2,b'TOOL\\xff\\n'); print('selected='+pathlib.Path.cwd().name)\n")
        rustup.chmod(0o755)
        for name in ("rustc", "cargo"):
            (bin_dir / name).symlink_to(rustup)
        for label in ("original", "snapshot"):
            folder = work / label
            folder.mkdir()
            for name in ("rustc", "cargo"):
                (folder / (name + ".toolchain")).write_text(label + name)
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode="w:"):
            pass
        try:
            ROOT = work / "snapshot"
            with mock.patch.dict(os.environ, {"PATH": str(bin_dir)}):
                manifest = snapshot_manifest("synthetic", "synthetic", archive.getvalue(), None)
            for name in ("rustc", "cargo"):
                selected = ROOT / (name + ".toolchain")
                entry = manifest["tools"][name]
                require(entry["stdout"] == "selected=snapshot\n" and
                        entry["stderr_hex"] == b"TOOL\xff\n".hex() and
                        entry["toolchain_executable"] == str(selected) and
                        entry["toolchain_sha256"] == hashlib.sha256(selected.read_bytes()).hexdigest(),
                        "manifest described the original directory or only the shim")
        finally:
            ROOT = repository
    print("PASS: timeout raw logs, snapshot failure receipts, reaper guards and selected tool identities")


def package_optimization_controls():
    """The independent checker must reject identical invalid bytes in all modes.

    These are synthetic archives of the actual allowlisted inputs, never a
    publication or a substitute for cargo's real package/privacy gate.
    """
    checker = ROOT / "docs/packaging/check_package.py"
    spec = importlib.util.spec_from_file_location("package_optimization_control", checker)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    valid = {name: (ROOT / ("Cargo.toml" if name == "Cargo.toml.orig" else name)).read_bytes()
             for name in module.EXPECTED}
    # A Cargo archive's generated manifest is distinct from Cargo.toml.orig.
    # Preserve that distinction in this hand-authored control, rather than
    # weakening the checker to accept the old raw-source stand-in.
    original = valid["Cargo.toml.orig"].decode()
    package, dependencies = original.split("[dependencies]\n", 1)
    package = package.replace("[package]\n", "[package]\n" +
                              "\n".join(f"{name} = false" for name in
                                        ("build", "autolib", "autobins", "autoexamples", "autotests", "autobenches")) + "\n", 1)
    dependencies = re.sub(r'^(\w+) = ("[^"\n]+")$', r'\1 = { version = \2 }', dependencies, flags=re.MULTILINE)
    valid["Cargo.toml"] = (package + "[dependencies]\n" + dependencies).encode()
    def archive(files):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as output:
            for name, payload in sorted(files.items()):
                member = tarfile.TarInfo("RunDelta-0.2.0-alpha.1/" + name)
                member.size = len(payload)
                output.addfile(member, io.BytesIO(payload))
        return data.getvalue()
    cases = {
        "valid": (valid, True),
        "inventory": (valid | {"target/synthetic-private.json": b"synthetic"}, False),
        "privacy": (valid | {"docs/CRATE_README.md": b"/home/synthetic-control/private"}, False),
        "license": (valid | {"LICENSE": b"wrong license"}, False),
    }
    repository_line = b'repository = "https://github.com/Dohyeon2006/rundelta"'
    require(all(valid[name].count(repository_line) == 1 for name in ("Cargo.toml", "Cargo.toml.orig")),
            "package control requires the independently fixed public repository URL")
    for label, replacement in (
        ("wrong_repository", b'repository = "https://example.invalid/other"'),
        ("credential_repository", b'repository = "https://' + b'user:password@example.invalid/repo"'),
        ("missing_repository", b""),
        ("homepage", repository_line + b'\nhomepage = "https://example.invalid/home"'),
        ("documentation", repository_line + b'\ndocumentation = "https://example.invalid/docs"'),
    ):
        changed = dict(valid)
        for name in ("Cargo.toml", "Cargo.toml.orig"):
            changed[name] = valid[name].replace(repository_line, replacement)
        cases[label] = (changed, False)
    with tempfile.TemporaryDirectory(prefix="package-optimization-control-") as directory:
        for name, (files, accepted) in cases.items():
            package = Path(directory) / (name + ".crate")
            package.write_bytes(archive(files))
            for options, environment in (([], {}), (["-O"], {}), ([], {"PYTHONOPTIMIZE": "1"})):
                result = run_owned_command([sys.executable, "-B", *options, str(checker), str(package)],
                                           env=gate_environment() | environment,
                                           capture_output=True, timeout=10)
                require((result.returncode == 0) == accepted,
                        f"checker mode changed validation: {name}, {options}, {environment}, {result.stderr!r}")
        result = run_owned_command([sys.executable, "-O", "-B", str(checker),
                                   str(Path(directory) / "valid.crate"), "--self-test"],
                                  env=gate_environment(), capture_output=True, timeout=10)
        require(result.returncode == 0, "optimized archive negative controls did not pass")
    print("PASS: standalone package validation in ordinary, -O and PYTHONOPTIMIZE modes")


def run_gates(
    core_only: bool,
    summary: list[dict[str, object]],
    *,
    exported: bytes | None = None,
    diff_command: list[str] | None = None,
    diff_cwd: Path | None = None,
    repository: Path | None = None,
    release: bool = False,
    evidence_dir: Path | None = None,
    check_timeout: float = DEFAULT_CHECK_TIMEOUT,
    gate_timeout: float = DEFAULT_GATE_TIMEOUT,
) -> None:
    started = time.monotonic()
    def check(name, command, reason=None, *, cwd=None):
        budget = min(check_timeout, gate_timeout - (time.monotonic() - started))
        run_check(summary, name, command, reason, cwd=cwd,
                  timeout=budget, evidence_dir=evidence_dir,
                  environment=STRICT_RUSTDOC_ENV if name == "rustdoc" else None)
        if exported is not None:
            verify_snapshot(exported, ROOT)
    for name, command in [
        ("format", ["cargo", "fmt", "--check"]),
        (
            "clippy",
            [
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
        ("unit_and_default_tests", ["cargo", "test", "--workspace"]),
        ("build", ["cargo", "build", "--workspace", "--all-targets", "--all-features"]),
        ("rustdoc", ["cargo", "doc", "--workspace", "--all-features", "--no-deps"]),
        ("audit_oracles", [sys.executable, "-B", "tests/full_gate.py", "--audit-oracles"]),
        ("cwd_tree_warning_contract", [sys.executable, "-B", "tests/check_cwd_tree_warning_contract.py"]),
        ("public_archive_freeze", [sys.executable, "-B", "tests/check_public_archive_freeze.py"]),
        ("diff_check", diff_command or ["git", "diff", "--check", "HEAD", "--"]),
    ]:
        check(name, command, cwd=diff_cwd if name == "diff_check" else None)

    if release:
        if time.monotonic() - started >= gate_timeout:
            fail_check(summary, "acceptance_inventory", "gate deadline exceeded", stage="deadline")
        else:
            with check_log(evidence_dir, "acceptance_inventory"):
                acceptance_inventory(summary, timeout=min(check_timeout, gate_timeout - (time.monotonic() - started)))
            attach_check_log(summary, evidence_dir, "acceptance_inventory")
    runner_reason = "--core-only selected" if core_only else capture_runner_reason()
    check("capture_runner_oracles", [sys.executable, "-B", "tests/full_gate.py", "--capture-runner-oracles"], runner_reason)
    namespace_reason = "--core-only selected" if core_only else (
        None if time.monotonic() - started >= gate_timeout else namespace_unshare_reason())
    check("namespace_unshare_regressions", [sys.executable, "-B", "tests/full_gate.py", "--namespace-unshare-oracles"], namespace_reason)
    for name in ACCEPTANCE_CHECKS:
        expired = time.monotonic() - started >= gate_timeout
        reason = "--core-only selected" if core_only else None if expired else acceptance_reason(name)
        check(
            name,
            [
                "cargo",
                "test",
                "--test",
                "acceptance",
                name,
                "--",
                "--ignored",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ],
            reason,
        )

    if core_only:
        skip_check(summary, "package_allowlist", "--core-only selected")
    else:
        try:
            if time.monotonic() - started >= gate_timeout:
                fail_check(summary, "package_allowlist", "gate deadline exceeded", stage="deadline")
            else:
                with check_log(evidence_dir, "package_allowlist"):
                    package_gate(summary, exported, repository=repository,
                                 timeout=min(check_timeout, gate_timeout - (time.monotonic() - started)))
                attach_check_log(summary, evidence_dir, "package_allowlist")
        except OSError as error:
            fail_check(
                summary,
                "package_allowlist",
                f"isolated package workspace failed: {error}",
                stage="isolated_workspace",
            )

def verify_snapshot(data: bytes, source: Path) -> None:
    """Gate commands may create target artifacts, never change candidate bytes."""
    expected = set()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as archive:
        for member in archive.getmembers():
            if member.isfile():
                expected.add(member.name)
                path = source / member.name
                payload = archive.extractfile(member)
                require(payload is not None, f"missing archive payload: {member.name}")
                require(path.is_file() and not path.is_symlink(),
                        f"candidate file was removed or replaced: {member.name}")
                parent = path.parent
                while parent != source:
                    require(not parent.is_symlink(), f"candidate parent became a symlink: {member.name}")
                    parent = parent.parent
                require(path.read_bytes() == payload.read(),
                        f"candidate bytes changed during validation: {member.name}")
                require(path.stat().st_mode & 0o777 == member.mode & 0o777,
                        f"candidate mode changed during validation: {member.name}")
    # Extra source/build scripts can influence a later command even if every
    # original file remains byte-identical. Only generated target artifacts are
    # admitted; PYTHONDONTWRITEBYTECODE avoids a cache exception outside target.
    for directory, children, files in os.walk(source, followlinks=False):
        here = Path(directory)
        for child in children:
            require(not (here / child).is_symlink(), f"candidate directory became a symlink: {here / child}")
        if here == source and "target" in children:
            children.remove("target")
        for name in files:
            path = here / name
            relative = path.relative_to(source).as_posix()
            require(relative in expected and not path.is_symlink(),
                    f"unexpected candidate file appeared: {relative}")


def candidate_gate(index: Path, core_only: bool, summary: list[dict[str, object]], *, release: bool = False,
                   evidence_dir: Path | None = None, check_timeout: float = DEFAULT_CHECK_TIMEOUT,
                   gate_timeout: float = DEFAULT_GATE_TIMEOUT) -> None:
    """Validate an explicit index tree without creating a commit or changing HEAD."""
    global ROOT
    repository = ROOT
    index = index.resolve(strict=True)
    default_index = checked_output(
        ["git", "rev-parse", "--git-path", "index"], cwd=repository, text=True
    ).strip()
    require(index != (repository / default_index).resolve(),
            "candidate mode requires an isolated index, not the user's index")
    environment = gate_environment() | {"GIT_INDEX_FILE": str(index)}
    tree = checked_output(
        ["git", "write-tree"], cwd=repository, env=environment, text=True
    ).strip()
    base = checked_output(
        ["git", "rev-parse", "HEAD"], cwd=repository, text=True
    ).strip()
    run_check(summary, "release_closure", [
        sys.executable, "-B", "tests/check_release_closure.py", "--tree", tree,
    ], timeout=check_timeout, evidence_dir=evidence_dir)
    if summary[-1]["status"] != "passed":
        return
    exported = checked_output(
        ["git", "archive", "--format=tar", tree], cwd=repository
    )
    snapshot_gate(tree, base, exported, core_only, summary, release=release,
                  evidence_dir=evidence_dir, check_timeout=check_timeout, gate_timeout=gate_timeout)
    unchanged = checked_output(
        ["git", "write-tree"], cwd=repository, env=environment, text=True
    ).strip()
    require(unchanged == tree, "isolated index changed during validation")
    append_result(summary, "candidate_snapshot", "passed", stage=tree)


def snapshot_gate(tree, base, exported, core_only, summary, *, release=False,
                  evidence_dir=None, check_timeout=DEFAULT_CHECK_TIMEOUT,
                  gate_timeout=DEFAULT_GATE_TIMEOUT):
    """Execute every product check against one pre-exported, immutable source tree."""
    global ROOT
    repository = ROOT
    previous_backend = os.environ.get("RUNDELTA_STRACE")
    backend = resolve_strace(repository)
    if backend is not None:
        os.environ["RUNDELTA_STRACE"] = str(backend)
    previous_target = os.environ.pop("CARGO_TARGET_DIR", None)
    if evidence_dir is not None:
        (evidence_dir / "source.tar").write_bytes(exported)
    try:
        with tempfile.TemporaryDirectory(prefix="rundelta-snapshot-gate-") as directory:
            ROOT = Path(directory)
            extract_clean_tree(exported, ROOT)
            if evidence_dir is not None:
                # Directory-specific rustup selection must match the cwd used
                # by the actual checks, rather than the original checkout.
                manifest = snapshot_manifest(tree, base, exported, backend)
                (evidence_dir / "inputs.json").write_text(json.dumps(manifest, indent=2) + "\n")
                verify_snapshot(exported, ROOT)
            print(f"SNAPSHOT tree={tree} base={base}", flush=True)
            run_gates(core_only, summary, exported=exported,
                      diff_command=["git", "diff", "--check", base, tree, "--"],
                      diff_cwd=repository, repository=repository, release=release,
                      evidence_dir=evidence_dir, check_timeout=check_timeout,
                      gate_timeout=gate_timeout)
            verify_snapshot(exported, ROOT)
            require(checked_output(["git", "rev-parse", "HEAD"],
                    cwd=repository, text=True).strip() == base,
                    "HEAD changed during snapshot validation")
            if evidence_dir is not None:
                binary = ROOT / "target/debug/rundelta"
                if binary.is_file():
                    saved_binary = evidence_dir / "rundelta-tested"
                    shutil.copyfile(binary, saved_binary)
                    saved_binary.chmod(0o700)
                    with saved_binary.open("rb") as stream:
                        binary_hash = hashlib.file_digest(stream, "sha256").hexdigest()
                    (evidence_dir / "binary.json").write_text(json.dumps({
                        "artifact": saved_binary.name, "sha256": binary_hash,
                        "build_command": ["cargo", "test", "--workspace"],
                        "source_tree": tree,
                    }, indent=2) + "\n")
    finally:
        ROOT = repository
        if previous_backend is None:
            os.environ.pop("RUNDELTA_STRACE", None)
        else:
            os.environ["RUNDELTA_STRACE"] = previous_backend
        if previous_target is not None:
            os.environ["CARGO_TARGET_DIR"] = previous_target


def snapshot_manifest(tree, base, exported, backend):
    """Record replay inputs; selected environment keys are explicit and private."""
    files = []
    with tarfile.open(fileobj=io.BytesIO(exported), mode="r:") as archive:
        for member in archive.getmembers():
            if member.isfile():
                files.append(dict(path=member.name, mode=member.mode,
                                  sha256=hashlib.sha256(archive.extractfile(member).read()).hexdigest()))
    tools = {}
    for name, command in {
        "rustc": ["rustc", "-Vv"], "cargo": ["cargo", "-V"],
        "python": [sys.executable, "-VV"], "cc": ["cc", "--version"],
        "strace": [str(backend), "-V"] if backend else [],
    }.items():
        if command:
            try:
                result = run_owned_command(command, cwd=ROOT, env=gate_environment(),
                                           capture_output=True, timeout=10)
                tools[name] = dict(command=command, returncode=result.returncode,
                                   stdout=result.stdout.decode("utf-8", errors="replace"),
                                   stderr=result.stderr.decode("utf-8", errors="replace"),
                                   stdout_hex=result.stdout.hex(), stderr_hex=result.stderr.hex())
            except subprocess.TimeoutExpired as error:
                tools[name] = dict(command=command, error=str(error),
                                   stdout_hex=(error.output or b"").hex(),
                                   stderr_hex=(error.stderr or b"").hex())
            except OSError as error:
                tools[name] = dict(command=command, error=str(error))
            executable_path = shutil.which(command[0])
            if executable_path is not None:
                resolved = Path(executable_path).resolve()
                with resolved.open("rb") as stream:
                    tools[name].update(executable=str(resolved), sha256=hashlib.file_digest(stream, "sha256").hexdigest())
                if name in ("rustc", "cargo") and resolved.name == "rustup":
                    # A rustup shim hash identifies the selector, not the compiler
                    # selected by this source directory and environment.
                    selection = run_owned_command([str(resolved), "which", name], cwd=ROOT,
                                                  env=gate_environment(), capture_output=True,
                                                  text=True, timeout=10, check=True)
                    selected = Path(selection.stdout.strip()).resolve(strict=True)
                    with selected.open("rb") as stream:
                        tools[name].update(toolchain_executable=str(selected),
                                           toolchain_sha256=hashlib.file_digest(stream, "sha256").hexdigest())
    if backend:
        with backend.open("rb") as stream:
            tools["strace"]["sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
    drivers = []
    for path in (Path(__file__).resolve(), Path(__file__).with_name("acceptance_support.py").resolve()):
        drivers.append(dict(path=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest()))
    return dict(schema_version=1, source_tree=tree, base_head=base, execution_drivers=drivers,
                source_archive_sha256=hashlib.sha256(exported).hexdigest(), files=files,
                tools=tools, kernel=dict(system=platform.system(), release=platform.release(),
                                        machine=platform.machine()),
                selected_environment={key: os.environ.get(key) for key in (
                    "RUNDELTA_STRACE", "RUNDELTA_STRACE_SOURCE", "RUNDELTA_RELEASE_GATE",
                    "RUSTFLAGS", "RUSTDOCFLAGS", "CARGO_TARGET_DIR", "CARGO_HOME", "RUSTUP_HOME",
                    "RUSTUP_TOOLCHAIN", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER")},
                invocation=sys.argv, privacy="local evidence; review before sharing")


def clean_snapshot_gate(core_only, summary, *, release=False, evidence_dir=None,
                        check_timeout=DEFAULT_CHECK_TIMEOUT, gate_timeout=DEFAULT_GATE_TIMEOUT):
    """Normal full/release mode must bind checks to a clean tree before any test."""
    base = checked_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    exported = clean_revision_archive(summary)
    if exported is None:
        return
    require(checked_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip() == base,
            "HEAD changed while acquiring clean snapshot")
    tree = checked_output(["git", "rev-parse", base + "^{tree}"], cwd=ROOT, text=True).strip()
    # Re-export by the frozen tree ID, never by the mutable HEAD reference.
    exported = checked_output(["git", "archive", "--format=tar", tree], cwd=ROOT)
    snapshot_gate(tree, base, exported, core_only, summary, release=release,
                  evidence_dir=evidence_dir, check_timeout=check_timeout, gate_timeout=gate_timeout)
    append_result(summary, "source_snapshot", "passed", stage=tree)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--core-only", action="store_true",
                      help="run core checks and explicitly skip environmental gates")
    mode.add_argument("--self-test", action="store_true",
                      help="exercise gate status and archive-safety control flow")
    mode.add_argument("--release", action="store_true",
                      help="require every gate and nested acceptance test, with zero skips")
    mode.add_argument("--audit-oracles", action="store_true", help="run all registered independent oracle suites strictly")
    mode.add_argument("--capture-runner-oracles", action="store_true", help="run the capture process ownership suite strictly")
    mode.add_argument("--namespace-unshare-oracles", action="store_true", help="run the separate namespace-only unshare suite strictly")
    parser.add_argument("--candidate-index", type=Path,
                        help="validate this isolated index's immutable tree before committing")
    parser.add_argument("--evidence-dir", type=Path, help="new local directory for snapshot, inputs, raw logs and summary")
    parser.add_argument("--check-timeout", type=float, default=DEFAULT_CHECK_TIMEOUT, help="maximum seconds for one gate check (default: 600)")
    parser.add_argument("--gate-timeout", type=float, default=DEFAULT_GATE_TIMEOUT, help="maximum seconds for the gate checks (default: 3600)")
    args = parser.parse_args()
    if args.capture_runner_oracles:
        if args.candidate_index:
            parser.error("--capture-runner-oracles cannot be combined with --candidate-index")
        return strict_oracles(("capture_runner",))
    if args.namespace_unshare_oracles:
        if args.candidate_index:
            parser.error("--namespace-unshare-oracles cannot be combined with --candidate-index")
        return strict_oracles(("namespace_unshare",))
    if args.audit_oracles:
        if args.candidate_index:
            parser.error("--audit-oracles cannot be combined with --candidate-index")
        return audit_oracles()
    if args.self_test:
        if args.candidate_index:
            parser.error("--self-test cannot be combined with --candidate-index")
        return self_test()
    if not all(math.isfinite(value) and value > 0 for value in (args.check_timeout, args.gate_timeout)):
        parser.error("deadlines must be finite and positive")
    evidence = args.evidence_dir or ROOT / "target/gate-evidence" / f"{time.time_ns()}-{os.getpid()}"
    evidence = evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    summary: list[dict[str, object]] = []
    if args.release:
        os.environ["RUNDELTA_RELEASE_GATE"] = "1"
    if args.candidate_index:
        try:
            candidate_gate(args.candidate_index, args.core_only, summary, release=args.release,
                           evidence_dir=evidence, check_timeout=args.check_timeout, gate_timeout=args.gate_timeout)
        except (OSError, ValueError, AssertionError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            fail_check(summary, "candidate_snapshot", str(error), stage="snapshot")
    elif args.core_only:
        run_gates(True, summary, evidence_dir=evidence,
                  check_timeout=args.check_timeout, gate_timeout=args.gate_timeout)
    else:
        try:
            clean_snapshot_gate(False, summary, release=args.release, evidence_dir=evidence,
                                check_timeout=args.check_timeout, gate_timeout=args.gate_timeout)
        except (OSError, ValueError, AssertionError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            fail_check(summary, "source_snapshot", str(error), stage="snapshot")
    if args.release:
        errors = release_errors(summary, candidate=args.candidate_index is not None)
        if errors:
            fail_check(summary, "release_policy", "; ".join(errors))
        else:
            append_result(summary, "release_policy", "passed")
    result = {"schema_version": 1, "checks": summary, "counts": counts(summary), "evidence_dir": str(evidence)}
    (evidence / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return exit_code(summary)


if __name__ == "__main__":
    raise SystemExit(main())
