#!/usr/bin/env python3
"""Adversarial supervised-backend fixture for version and capture boundaries."""

from __future__ import annotations

import os
from pathlib import Path
import signal
import sys
import tempfile
import time


def write_marker(variable: str, value: str) -> None:
    destination = os.environ.get(variable)
    if destination is not None:
        target = Path(destination)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w", encoding="ascii", dir=target.parent,
                prefix=f".{target.name}.", suffix=".tmp", delete=False,
            ) as marker:
                temporary = Path(marker.name)
                marker.write(value)
            # Publish the closed, complete inode without replacing any existing entry.
            os.link(temporary, target)
        finally:
            primary_error = sys.exc_info()[0] is not None
            if temporary is not None:
                try:
                    temporary.unlink()
                except FileNotFoundError:
                    pass
                except OSError:
                    if not primary_error:
                        raise


def write_all(descriptor: int, data: bytes) -> None:
    while data:
        written = os.write(descriptor, data)
        data = data[written:]


def version_probe(mode: str) -> int:
    write_marker("RF440_READY", f"{os.getpid()}\n")
    release = os.environ.get("RF440_RELEASE")
    while release is not None and not Path(release).exists():
        time.sleep(0.005)
    if mode == "success":
        write_all(1, b"fixture-strace 1.0\nignored second line\n")
        return 0
    if mode == "hang":
        while True:
            time.sleep(60)
    if mode == "flood":
        block = b"x" * 8192
        for _ in range(9):
            write_all(1, block)
            write_all(2, block)
        return 0
    if mode == "nonzero":
        write_all(2, b"BEGIN-STDERR\n" + b"e" * 10_000 + b"\nEND-STDERR\n")
        return 23
    if mode == "self-signal":
        signal.signal(signal.SIGTERM, signal.SIG_DFL)
        os.kill(os.getpid(), signal.SIGTERM)
        return 99
    raise RuntimeError(f"unknown RF440 probe mode: {mode}")


def trace_path(arguments: list[str]) -> Path:
    try:
        index = arguments.index("-o")
        return Path(arguments[index + 1])
    except (ValueError, IndexError) as error:
        raise RuntimeError("collector fixture did not receive -o TRACE") from error


def capture_probe(mode: str, arguments: list[str]) -> int:
    output = trace_path(arguments)
    size = int(os.environ.get("RF450_TRACE_BYTES", "0"))
    term_ready = os.environ.get("RF450_TERM_READY")
    if mode in ("trace-hang", "trace-live"):

        def observe_term(_number: int, _frame: object) -> None:
            if term_ready is not None:
                write_marker("RF450_TERM_READY", f"{os.getpid()}\n")

        signal.signal(signal.SIGTERM, observe_term)
    if mode == "trace-live":
        # Keep the raw producer genuinely live while the test controller denies
        # termination/reap. This synthetic backend never executes its target.
        with output.open("wb", buffering=0) as trace:
            write_marker("RF450_COLLECTOR_READY", f"{os.getpid()}\n")
            while True:
                trace.write(b'7 open("/synthetic/live", O_RDONLY) = -1 ENOENT (No file)\n')
                time.sleep(0.01)
    with output.open("wb", buffering=0) as trace:
        if mode == "late-invalid-utf8":
            trace.write(
                b'7 execve("/tool", ["tool"], 0x0) = 0\n'
                b"7 +++ exited with 0 +++\n"
                b"\xff\n"
            )
        else:
            trace.write(b"x" * size)
    write_marker("RF450_COLLECTOR_READY", f"{os.getpid()}\n")
    if mode in ("trace-exit", "late-invalid-utf8"):
        return 0
    if mode == "trace-hang":
        while True:
            time.sleep(60)
    raise RuntimeError(f"unknown RF450 collector mode: {mode}")


def main() -> int:
    if "--version" in sys.argv[1:]:
        return version_probe(os.environ.get("RF440_PROBE_MODE", "success"))
    capture_mode = os.environ.get("RF450_COLLECTOR_MODE")
    if capture_mode is not None:
        return capture_probe(capture_mode, sys.argv[1:])
    backend = os.environ["RF440_REAL_STRACE"]
    # Python ignores SIGPIPE at runtime; do not let this test adapter alter the
    # existing Rust Command/strace target boundary characterized elsewhere.
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    os.execv(backend, [backend, *sys.argv[1:]])
    return 98


if __name__ == "__main__":
    raise SystemExit(main())
