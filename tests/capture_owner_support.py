"""External syscall faults for a clean recorder image, never a CLI test hook.

The tracer owns its forked subject. It reads kernel-classified syscall stops,
not a guessed entry/exit toggle, and preserves separate product channels.
Every invocation retains an exact receipt before execution and its raw result.
"""

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]


def fingerprint(path):
    path = Path(path)
    return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def save(path, value):
    with Path(path).open("x", encoding="ascii") as output:
        json.dump(value, output, ensure_ascii=True, indent=2)
        output.write("\n")


def compile_probe(argv, *, cwd, evidence):
    """Bind native test sources and compiler before each actual compilation."""
    folder = Path(evidence)
    folder.mkdir(mode=0o700, parents=True, exist_ok=False)
    argv = list(map(str, argv))
    argv[0] = str(Path(shutil.which(argv[0])).resolve())
    save(folder / "before.json", {
        "argv": argv, "cwd": str(cwd), "compiler": fingerprint(argv[0]),
        "sources": [fingerprint(value) for value in argv[1:] if value.endswith(".c")],
    })
    result = subprocess.run(argv, cwd=cwd, capture_output=True, timeout=30)
    (folder / "stdout").write_bytes(result.stdout)
    (folder / "stderr").write_bytes(result.stderr)
    artifact = argv[argv.index("-o") + 1]
    save(folder / "result.json", {"returncode": result.returncode,
                                "artifact": fingerprint(artifact) if result.returncode == 0 else None})
    if result.returncode:
        raise subprocess.CalledProcessError(result.returncode, argv, result.stdout, result.stderr)
    return result


class FaultBroker:
    """Own one compiled probe and append-only invocation directories."""

    def __init__(self, work):
        if platform.system() != "Linux" or platform.machine() != "x86_64":
            raise RuntimeError("the syscall fault probe requires Linux x86_64")
        self.work = Path(work)
        self.work.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.binary = self.work / "capture-syscall"
        source = ROOT / "tests/probes/capture_syscall.c"
        compiler = Path(shutil.which("cc")).resolve()
        command = [str(compiler), "-Wall", "-Wextra", "-Werror", "-O2", str(source), "-o", str(self.binary)]
        save(self.work / "compile-before.json", {"argv": command, "cwd": str(ROOT), "source": fingerprint(source), "compiler": fingerprint(compiler)})
        result = subprocess.run(command, cwd=ROOT, capture_output=True, timeout=30)
        (self.work / "compile.stdout").write_bytes(result.stdout)
        (self.work / "compile.stderr").write_bytes(result.stderr)
        save(self.work / "compile-result.json", {"returncode": result.returncode})
        if result.returncode:
            raise AssertionError(result.stderr)
        self.calls = 0
        # These actual controls fail if ptrace is refused or the fault controller
        # merely claims injection without making the subject observe EIO.
        for mode, expected in (("none", 1), ("getpid", 0)):
            result = self.run([str(self.binary), "_subject"], self.work / "control-storage", mode)
            if result.returncode != expected:
                raise AssertionError(("syscall broker control", mode, result))

    def prepare(self, command, storage, mode, environment, cwd):
        self.calls += 1
        folder = self.work / f"call-{self.calls:04d}"
        folder.mkdir(mode=0o700)
        output, errors, events = (folder / label for label in ("product.stdout", "product.stderr", "events.txt"))
        argv = [str(self.binary), mode, str(storage), str(output), str(errors), str(events), "--", *map(str, command)]
        env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1", **(environment or {})}
        save(folder / "before.json", {
            "argv": argv, "argv_bytes_hex": [os.fsencode(value).hex() for value in argv],
            "cwd": str(cwd), "cwd_bytes_hex": os.fsencode(cwd).hex(),
            "environment": {key: value for key, value in env.items() if key.startswith(("RUNDELTA", "CAPTURE_FAULT", "LD_", "RD_", "RF"))},
            "tools": [fingerprint(self.binary), fingerprint(command[0])],
            "at_ns": time.time_ns(), "scope": "new test-owned subject only; no global ptrace/security-policy changes",
        })
        return folder, argv, env

    def result(self, folder, outer, elapsed_ns):
        # Callers may supervise a barrier with Popen rather than run(). Keep the
        # controller's own channels in this single terminal path in both cases,
        # including a contradictory/failed controller result before rejecting it.
        for name, data in (("broker.stdout", outer.stdout), ("broker.stderr", outer.stderr)):
            with (folder / name).open("xb") as output:
                output.write(data)
        events = (folder / "events.txt").read_text()
        facts = re.findall(r"subject_returncode=(\d+) raw_wait_status=(\d+)", events)
        raw_status = int(facts[0][1]) if len(facts) == 1 else None
        code = os.waitstatus_to_exitcode(raw_status) if raw_status is not None else None
        expected = code if code is not None and code >= 0 else (128 - code if code is not None else None)
        coherent = len(facts) == 1 and int(facts[0][0]) == expected and outer.returncode == expected
        save(folder / "result.json", {
            "broker_returncode": outer.returncode, "product_returncode": code,
            "raw_wait_status": raw_status, "elapsed_ns": elapsed_ns,
            "stdout": fingerprint(folder / "product.stdout"), "stderr": fingerprint(folder / "product.stderr"),
            "events": fingerprint(folder / "events.txt"),
            "broker_stdout": fingerprint(folder / "broker.stdout"),
            "broker_stderr": fingerprint(folder / "broker.stderr"),
            "broker_and_actual_wait_agree": coherent,
        })
        if not coherent:
            raise AssertionError(("fault controller failure or contradictory final wait", outer.returncode, events, outer.stderr))
        result = subprocess.CompletedProcess(outer.args, code,
                                             (folder / "product.stdout").read_bytes(),
                                             (folder / "product.stderr").read_bytes())
        result.evidence = folder
        return result

    def run(self, command, storage, mode="none", *, environment=None, cwd=ROOT, runner=None):
        if runner and Path(cwd) != runner.cwd:
            raise ValueError("fault receipt cwd differs from the isolated runner's actual cwd")
        folder, argv, env = self.prepare(command, storage, mode, environment, cwd)
        start = time.monotonic_ns()
        outer = (runner.run(argv, environment=env) if runner else
                 subprocess.run(argv, env=env, cwd=cwd, capture_output=True, timeout=30))
        return self.result(folder, outer, time.monotonic_ns() - start)

    @staticmethod
    def denials(result):
        return len(re.findall(r"^event=deny ", (result.evidence / "events.txt").read_text(), re.M))

    @staticmethod
    def committed(result):
        return Path(str(result.evidence / "events.txt") + ".committed-snapshot")


class BrokerReceiptTests(unittest.TestCase):
    """Break a controller that masks its own failure as a successful subject."""

    def test_actual_wait_and_broker_status_must_both_agree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, (raw, declared, broker, valid) in enumerate([
                (0, 0, 0, True), (2 << 8, 2, 2, True), (9, 137, 137, True),
                (0, 0, 2, False), (2 << 8, 0, 0, False), (9, 137, 2, False),
            ]):
                folder = root / str(index)
                folder.mkdir()
                for name in ("product.stdout", "product.stderr"):
                    (folder / name).write_bytes(b"")
                (folder / "events.txt").write_text(f"subject_returncode={declared} raw_wait_status={raw}\n")
                controller = object.__new__(FaultBroker)
                outer = subprocess.CompletedProcess(["controller"], broker, b"raw controller stdout\xff", b"raw controller stderr\xfe")
                if valid:
                    self.assertEqual(controller.result(folder, outer, 1).returncode, os.waitstatus_to_exitcode(raw))
                else:
                    with self.assertRaisesRegex(AssertionError, "contradictory final wait"):
                        controller.result(folder, outer, 1)
                self.assertEqual(json.loads((folder / "result.json").read_text())["broker_and_actual_wait_agree"], valid)
                self.assertEqual((folder / "broker.stdout").read_bytes(), outer.stdout)
                self.assertEqual((folder / "broker.stderr").read_bytes(), outer.stderr)
