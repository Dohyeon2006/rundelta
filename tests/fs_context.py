#!/usr/bin/env python3
"""Real Linux capture regression for per-FsContext CWD modeling.

The probe uses inherited pipes to order each directory switch before the
reader's relative open. It creates only fresh records under target/.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from acceptance_support import main, strace_environment

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("RUNDELTA_BIN", ROOT / "target/debug/rundelta")).resolve()
ENV = strace_environment(ROOT)
WORK = Path(tempfile.mkdtemp(prefix="fs-context-", dir=ROOT / "target"))
PROBE = WORK / "probe"
LEGACY_CLONE_PROBE = WORK / "legacy-clone-parent"
STORE = WORK / "records"

# CLONE_PARENT must run in a child so its parent can reap both descendants.
# Use libc's legacy clone wrapper to avoid architecture-specific raw argument
# order; the captured syscall must still spell CLONE_PARENT|SIGCHLD.
LEGACY_CLONE_SOURCE = r"""#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

struct result { pid_t child; int error; };
static int child_main(void *unused) { (void)unused; return 0; }

int main(void) {
    int channel[2];
    if (pipe(channel) != 0) return 2;
    pid_t middle = fork();
    if (middle < 0) return 3;
    if (middle == 0) {
        unsigned char stack[65536];
        close(channel[0]);
        pid_t child = clone(child_main, stack + sizeof(stack),
                            CLONE_PARENT | SIGCHLD, NULL);
        struct result result = {child, child < 0 ? errno : 0};
        ssize_t sent;
        do { sent = write(channel[1], &result, sizeof(result)); }
        while (sent < 0 && errno == EINTR);
        close(channel[1]);
        _exit(child > 0 && sent == sizeof(result) ? 0 : 4);
    }
    close(channel[1]);
    struct result result = {-1, 0};
    size_t received = 0;
    while (received < sizeof(result)) {
        ssize_t count = read(channel[0], (char *)&result + received,
                             sizeof(result) - received);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) break;
        received += (size_t)count;
    }
    close(channel[0]);
    unsigned reaped = 0;
    int statuses_ok = 1;
    for (;;) {
        int status;
        pid_t child = waitpid(-1, &status, 0);
        if (child < 0 && errno == EINTR) continue;
        if (child < 0) break;
        ++reaped;
        statuses_ok &= WIFEXITED(status) && WEXITSTATUS(status) == 0;
    }
    printf("legacy CLONE_PARENT|SIGCHLD: child=%ld error=%d reaped=%u\n",
           (long)result.child, result.error, reaped);
    return received == sizeof(result) && result.child > 0 &&
           reaped == 2 && statuses_ok ? 0 : 5;
}
"""




class FsContext(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        subprocess.run(
            ["cc", "-Wall", "-Wextra", "-Werror", "-O2", "-pthread",
             str(ROOT / "tests/fixtures/fs_context_probe.c"), "-o", str(PROBE)],
            check=True,
        )
        legacy_source = WORK / "legacy-clone-parent.c"
        legacy_source.write_text(LEGACY_CLONE_SOURCE)
        subprocess.run(
            ["cc", "-Wall", "-Wextra", "-Werror", "-O2",
             str(legacy_source), "-o", str(LEGACY_CLONE_PROBE)],
            check=True,
        )
        for name, marker in [("A", "A"), ("B", "B")]:
            directory = WORK / name
            directory.mkdir()
            (directory / "payload").write_text(marker)

    def cli(self, *args, **kwargs):
        return subprocess.run(
            [str(BIN), "--storage", str(STORE), *args],
            cwd=WORK / "A",
            env=ENV,
            capture_output=True,
            text=True,
            timeout=30,
            **kwargs,
        )

    def record_probe(self, name, mode, operation, expected, expected_capture="complete"):
        destination = WORK / ("missing" if operation == "fail-chdir" else "B")
        recorded = self.cli("record", name, "--", str(PROBE), mode, operation, str(destination), expected)
        self.assertEqual(recorded.returncode, 0, (name, recorded.stderr))
        shown = self.cli("show", name, "--json")
        self.assertEqual(shown.returncode, 0, shown.stderr)
        run = json.loads(shown.stdout)
        metadata = run["metadata"]
        self.assertEqual(metadata["state"], "completed")
        actual_capture = "complete" if metadata["completeness"] == "complete_for_supported_events" else "incomplete"
        self.assertEqual(actual_capture, expected_capture, (name, metadata["warnings"]))
        payload = str((WORK / ("B" if expected == "B" else "A") / "payload"))
        events = [event for event in run["events"]
                  if event["kind"] == "file" and event["operation"] in ("open", "openat", "openat2")
                  and event["path"] == payload]
        self.assertTrue(events, (name, payload, run["events"]))
        observed = events[-1]
        self.assertEqual(observed["cwd_before"], str(WORK / ("B" if expected == "B" else "A")))
        self.assertFalse(observed["context_uncertain"], observed)
        text = self.cli("show", name)
        self.assertEqual(text.returncode, 0, text.stderr)
        self.assertIn("Last observed CWD", text.stdout)
        self.assertIn(observed["cwd_before"], text.stdout)
        report = self.cli("diff", name, name, "--json")
        text_report = self.cli("diff", name, name)
        expected_rc = 0 if expected_capture == "complete" else 3
        self.assertEqual((report.returncode, text_report.returncode), (expected_rc, expected_rc))
        data = json.loads(report.stdout)
        self.assertFalse(data["observed_differences"])
        self.assertEqual(data["reliability"]["left"], {
            "capture": expected_capture,
            "storage": "verified",
        })
        if expected_rc == 3:
            self.assertIn("RELIABILITY NOT ESTABLISHED", text_report.stdout)
        else:
            self.assertNotIn("RELIABILITY NOT ESTABLISHED", text_report.stdout)
        return run

    def test_shared_private_and_mixed_contexts(self):
        normal = self.record_probe("normal", "normal", "none", "A")
        self.assertEqual(normal["storage_integrity"]["status"], "verified")
        for name, mode, expected in [
            ("pthread", "pthread", "B"),
            ("clonefs", "clonefs", "B"),
            ("fork", "fork", "A"),
            ("mixed", "mixed", "A"),
        ]:
            with self.subTest(name=name):
                run = self.record_probe(name, mode, "chdir", expected)
                self.assertFalse(any("shared filesystem state" in warning for warning in run["metadata"]["warnings"]))
                self.assertFalse(any("fs.context" in warning for warning in run["metadata"]["warnings"]))

    def test_unshare_exec_and_failed_switches(self):
        for name, mode, operation, expected, capture in [
            ("unshare", "unshare", "chdir", "A", "complete"),
            ("exec-shared", "exec-shared", "chdir", "B", "complete"),
            ("exec-private", "exec-private", "chdir", "A", "complete"),
            ("failed-chdir", "pthread", "fail-chdir", "A", "complete"),
            # Existing fchdir operand-evidence semantics remain explicit: a
            # failed anonymous descriptor is partial, but does not change CWD.
            ("failed-fchdir", "pthread", "fail-fchdir", "A", "incomplete"),
            ("fchdir", "clonefs", "fchdir", "B", "complete"),
        ]:
            with self.subTest(name=name):
                self.record_probe(name, mode, operation, expected, capture)

    def test_missing_or_unparseable_clone_flags_remain_incomplete(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        for name, call in [
            ("missing-flags", "clone(child_stack=NULL)"),
            ("unparseable-flags", "clone(flags=???)"),
        ]:
            with self.subTest(name=name):
                trace = WORK / f"{name}.trace"
                trace.write_text("\n".join([
                    '20 execve("/synthetic/target", [], 0) = 0',
                    f"20 {call} = 21",
                    '21 +++ exited with 0 +++',
                    '20 +++ exited with 0 +++',
                    "",
                ]))
                env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}
                recorded = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "record", name, "--", "/synthetic/target"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(recorded.returncode, 0, recorded.stderr)
                shown = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "show", name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                run = json.loads(shown.stdout)
                self.assertEqual(run["metadata"]["completeness"], "partial")
                self.assertTrue(any("fs.context" in warning for warning in run["metadata"]["warnings"]))
                report = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "diff", name, name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                text_report = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "diff", name, name],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual((report.returncode, text_report.returncode), (3, 3))
                data = json.loads(report.stdout)
                self.assertEqual(data["reliability"]["left"], {"capture": "incomplete", "storage": "verified"})
                self.assertIn("RELIABILITY NOT ESTABLISHED", text_report.stdout)

    def test_legacy_clone_parent_signal_is_accepted_on_linux(self):
        recorded = self.cli("record", "legacy-clone-parent", "--", str(LEGACY_CLONE_PROBE))
        self.assertEqual(recorded.returncode, 0, recorded.stderr)
        self.assertIn("error=0 reaped=2", recorded.stdout)
        shown = self.cli("show", "legacy-clone-parent", "--json")
        self.assertEqual(shown.returncode, 0, shown.stderr)
        run = json.loads(shown.stdout)
        metadata = run["metadata"]
        self.assertEqual(metadata["state"], "completed")
        self.assertEqual(metadata["completeness"], "complete_for_supported_events")
        self.assertEqual(metadata["exit_code"], 0)
        self.assertEqual(metadata["collector_exit_code"], 0)
        self.assertFalse(metadata["warnings"], metadata["warnings"])
        # Inspect only this fresh private raw artifact to prove the kernel call
        # used the audited flags. The normalized event retains that exact row.
        raw = (STORE / "runs" / metadata["id"] / "raw/trace.log").read_text().splitlines()
        parent_clones = [event for event in run["events"]
                         if event["operation"] == "clone"
                         and "CLONE_PARENT|SIGCHLD" in raw[event["evidence"]["line"] - 1]]
        self.assertEqual(len(parent_clones), 1, raw)
        event = parent_clones[0]
        self.assertEqual(event["outcome"], "success")
        self.assertFalse(event["context_uncertain"])
        self.assertEqual(event["evidence"]["file"], "raw/trace.log")
        report = self.cli("diff", "legacy-clone-parent", "legacy-clone-parent", "--json")
        text_report = self.cli("diff", "legacy-clone-parent", "legacy-clone-parent")
        self.assertEqual((report.returncode, text_report.returncode), (0, 0))
        self.assertFalse(json.loads(report.stdout)["observed_differences"])

    def test_clone3_signal_conflict_and_strict_string_boundaries(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        # These impossible clone3-success and malformed-string rows are explicit
        # negative controls, not claims about trace output from a real kernel.
        cases = [
            ("clone3-parent-zero-signal", [
                '20 clone3({flags=CLONE_PARENT, exit_signal=0}, 88) = 21',
                '21 +++ exited with 0 +++',
            ], "complete_for_supported_events", None, "spawn"),
            ("clone3-thread-zero-signal", [
                '20 clone3({flags=CLONE_VM|CLONE_SIGHAND|CLONE_THREAD, exit_signal=0}, 88) = 21',
                '21 +++ exited with 0 +++',
            ], "complete_for_supported_events", None, "spawn"),
            ("clone3-parent-signal-conflict", [
                '20 clone3({flags=CLONE_PARENT, exit_signal=SIGCHLD}, 88) = 21',
                '21 +++ exited with 0 +++',
            ], "partial", "fs.context", "spawn"),
            ("clone3-thread-signal-conflict", [
                '20 clone3({flags=CLONE_VM|CLONE_SIGHAND|CLONE_THREAD, exit_signal=SIGCHLD}, 88) = 21',
                '21 +++ exited with 0 +++',
            ], "partial", "fs.context", "spawn"),
            ("encoded-interior-quote", [
                r'20 access("/a\x22b", F_OK) = 0',
            ], "complete_for_supported_events", None, "file"),
            ("unescaped-interior-quote", [
                '20 access("/a""b", F_OK) = 0',
            ], "partial", "unsupported or incomplete event", None),
            ("literal-string-ellipsis", [
                r'20 execve("/tool...name", ["argument...value", "\x22...\x22"], 0) = 0',
            ], "complete_for_supported_events", None, "exec"),
            ("abbreviated-argument-array", [
                '20 execve("/tool", ["arg", ...], 0) = 0',
            ], "partial", "abbreviated trace data", "exec"),
        ]
        for name, observations, completeness, warning_fragment, kind in cases:
            with self.subTest(name=name):
                trace = WORK / f"{name}.trace"
                trace.write_text("\n".join([
                    '20 execve("/synthetic/target", [], 0) = 0',
                    *observations,
                    '20 +++ exited with 0 +++',
                    "",
                ]))
                env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}
                def call(*args):
                    return subprocess.run(
                        [str(BIN), "--storage", str(STORE), *args],
                        cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                    )
                recorded = call("record", name, "--", "/synthetic/target")
                self.assertEqual(recorded.returncode, 0, recorded.stderr)
                shown = call("show", name, "--json")
                self.assertEqual(shown.returncode, 0, shown.stderr)
                run = json.loads(shown.stdout)
                self.assertEqual(run["metadata"]["state"], "completed")
                self.assertEqual(run["metadata"]["completeness"], completeness)
                warnings = run["metadata"]["warnings"]
                if warning_fragment is None:
                    self.assertFalse(warnings, warnings)
                else:
                    self.assertTrue(any(warning.startswith("raw/trace.log:2:")
                                        and warning_fragment in warning for warning in warnings), warnings)
                observed = [event for event in run["events"]
                            if event["evidence"]["line"] == 2]
                if kind is None:
                    self.assertFalse(observed)
                else:
                    self.assertEqual(len(observed), 1)
                    self.assertEqual(observed[0]["kind"], kind)
                    self.assertEqual(observed[0]["evidence"], {
                        "file": "raw/trace.log", "line": 2, "end_line": 2,
                    })
                    if kind == "spawn":
                        self.assertEqual(observed[0]["context_uncertain"], completeness == "partial")
                    elif name == "encoded-interior-quote":
                        self.assertEqual(observed[0]["path"], '/a"b')
                    elif name == "literal-string-ellipsis":
                        self.assertEqual(observed[0]["path"], "/tool...name")
                        self.assertEqual(observed[0]["argv"], ["argument...value", '"..."'])
                    else:
                        self.assertIsNone(observed[0]["argv"])
                report = call("diff", name, name, "--json")
                text_report = call("diff", name, name)
                expected_rc = 0 if completeness.startswith("complete") else 3
                self.assertEqual((report.returncode, text_report.returncode), (expected_rc, expected_rc))
                comparison = json.loads(report.stdout)
                self.assertFalse(comparison["observed_differences"])
                self.assertEqual(comparison["reliability"]["left"], {
                    "capture": "complete" if expected_rc == 0 else "incomplete",
                    "storage": "verified",
                })

    def test_directory_changes_require_absolute_per_syscall_evidence(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        cases = [
            (
                "absolute-chdir-evidence",
                '20 chdir("/known") = 0',
                "/known",
                "complete_for_supported_events",
                False,
                False,
            ),
            (
                "annotated-fchdir-evidence",
                r"20 fchdir(3<\x2f\x6b\x6e\x6f\x77\x6e>) = 0",
                "/known",
                "complete_for_supported_events",
                False,
                False,
            ),
            (
                "relative-chdir-success",
                '20 chdir("child") = 0',
                None,
                "partial",
                True,
                True,
            ),
            (
                "relative-chdir-failure",
                '20 chdir("child") = -1 ENOENT (No file)',
                str(WORK / "A"),
                "partial",
                True,
                False,
            ),
        ]
        for name, change, expected_cwd, completeness, change_uncertain, probe_uncertain in cases:
            with self.subTest(name=name):
                trace = WORK / f"{name}.trace"
                trace.write_text("\n".join([
                    '20 execve("/synthetic/target", [], 0) = 0',
                    change,
                    '20 open("/absolute-probe", O_RDONLY) = 3',
                    '20 +++ exited with 0 +++',
                    "",
                ]))
                env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}
                recorded = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "record", name, "--", "/synthetic/target"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(recorded.returncode, 0, recorded.stderr)
                shown = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "show", name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(shown.returncode, 0, shown.stderr)
                run = json.loads(shown.stdout)
                self.assertEqual(run["metadata"]["completeness"], completeness)
                change_event = next(event for event in run["events"]
                                    if event["evidence"]["line"] == 2)
                probe = next(event for event in run["events"]
                             if event["evidence"]["line"] == 3)
                self.assertEqual(change_event["context_uncertain"], change_uncertain)
                self.assertEqual(probe["cwd_before"], expected_cwd)
                self.assertEqual(probe["context_uncertain"], probe_uncertain)
                if "relative-chdir" in name:
                    self.assertEqual(change_event["path"], "relative:child [cwd unresolved]")
                    self.assertTrue(any(warning.startswith("raw/trace.log:2:")
                                        for warning in run["metadata"]["warnings"]))
                else:
                    self.assertFalse(run["metadata"]["warnings"])
                report = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "diff", name, name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(report.returncode, 0 if completeness.startswith("complete") else 3)
                self.assertFalse(json.loads(report.stdout)["observed_differences"])

    def test_return_value_fact_controls_success_and_completeness(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        # Negative controls are malformed synthetic input, not kernel traces.
        for operation in ("chdir", "unshare"):
            for result, malformed in [
                ("0", False),
                ("-1 EPERM (Operation not permitted)", False),
                ("-1 success", True),
            ]:
                name = f"return-fact-{operation}-{'malformed' if malformed else 'ok' if result == '0' else 'errno'}"
                with self.subTest(name=name):
                    prefix = ['20 execve("/synthetic/target", [], 0) = 0']
                    if operation == "unshare":
                        prefix.append('20 clone(flags=CLONE_FS|SIGCHLD) = 21')
                        call = f'21 unshare(CLONE_FS) = {result}'
                        suffix = ['21 chdir("/changed") = 0', '21 +++ exited with 0 +++']
                        expected_cwd = str(WORK / "A") if result == "0" else "/changed"
                    else:
                        call = f'20 chdir("/changed") = {result}'
                        suffix = []
                        expected_cwd = "/changed" if result == "0" else str(WORK / "A")
                    call_line = len(prefix) + 1
                    probe_line = call_line + len(suffix) + 1
                    raw = "\n".join([
                        *prefix, call, *suffix,
                        '20 open("/absolute-probe", O_RDONLY) = 3',
                        '20 +++ exited with 0 +++', "",
                    ])
                    trace = WORK / f"{name}.trace"
                    trace.write_text(raw)
                    env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}
                    def call_cli(*args):
                        return subprocess.run(
                            [str(BIN), "--storage", str(STORE), *args],
                            cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                        )
                    recorded = call_cli("record", name, "--", "/synthetic/target")
                    self.assertEqual(recorded.returncode, 0, recorded.stderr)
                    shown = call_cli("show", name, "--json")
                    self.assertEqual(shown.returncode, 0, shown.stderr)
                    run = json.loads(shown.stdout)
                    metadata = run["metadata"]
                    self.assertEqual(metadata["schema_version"], 3)
                    self.assertEqual(metadata["state"], "completed")
                    self.assertEqual(metadata["exit_code"], 0)
                    self.assertIsNone(metadata["signal"])
                    self.assertEqual(metadata["collector_exit_code"], 0)
                    self.assertEqual(metadata["completeness"], "partial" if malformed else "complete_for_supported_events")
                    self.assertEqual((STORE / "runs" / metadata["id"] / "raw/trace.log").read_text(), raw)
                    affected = [e for e in run["events"] if e["evidence"]["line"] == call_line]
                    self.assertEqual(len(affected), 0 if malformed else 1)
                    if affected:
                        self.assertEqual(affected[0]["outcome"], "success" if result == "0" else "EPERM")
                    warnings = metadata["warnings"]
                    self.assertEqual(len(warnings), 1 if malformed else 0, warnings)
                    if malformed:
                        self.assertTrue(warnings[0].startswith(f"raw/trace.log:{call_line}: unsupported or incomplete event:"))
                    probe = next(e for e in run["events"] if e["evidence"]["line"] == probe_line)
                    self.assertEqual(probe["schema_version"], 2)
                    self.assertEqual(probe["evidence"], {"file": "raw/trace.log", "line": probe_line, "end_line": probe_line})
                    self.assertEqual(probe["cwd_before"], expected_cwd)
                    self.assertFalse(probe["context_uncertain"])
                    data, text = call_cli("diff", name, name, "--json"), call_cli("diff", name, name)
                    self.assertEqual((data.returncode, text.returncode), (3, 3) if malformed else (0, 0))
                    comparison = json.loads(data.stdout)
                    self.assertFalse(comparison["observed_differences"])
                    self.assertEqual(comparison["reliability"]["left"], {
                        "capture": "incomplete" if malformed else "complete", "storage": "verified",
                    })
                    self.assertEqual("RELIABILITY NOT ESTABLISHED" in text.stdout, malformed)

    def test_namespace_unshare_does_not_invent_fs_separation(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        namespace_only = ["CLONE_NEWTIME", "CLONE_NEWCGROUP", "CLONE_NEWUTS",
                          "CLONE_NEWIPC", "CLONE_NEWPID", "CLONE_NEWNET"]
        cases = [(flags, result, False) for flags in namespace_only
                 for result in ("0", "-1 EPERM (denied)")]
        cases += [(flags, "0", True) for flags in (
            "CLONE_FS|CLONE_NEWNET", "CLONE_NEWUSER", "CLONE_NEWNS", "UNSHARE_EMPTY_MNTNS",
        )]
        cases += [("CLONE_NEWNET|CLONE_FUTURE", "0", False)]
        for number, (flags, result, split) in enumerate(cases):
            name = f"unshare-ns-scope-{number}"
            with self.subTest(flags=flags, result=result):
                trace = WORK / f"{name}.trace"
                raw = "\n".join([
                    '1 execve("/synthetic/target", [], 0) = 0',
                    '1 fork() = 3',
                    '1 clone(flags=CLONE_FS|SIGCHLD) = 2',
                    f'2 unshare({flags}) = {result}',
                    '2 chdir("/B") = 0',
                    '1 open("/parent", O_RDONLY) = 3',
                    '2 open("/caller", O_RDONLY) = 3',
                    '3 open("/private", O_RDONLY) = 3',
                    '2 +++ exited with 0 +++',
                    '3 +++ exited with 0 +++',
                    '1 +++ exited with 0 +++', "",
                ])
                trace.write_text(raw)
                env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}
                def call(*args):
                    return subprocess.run(
                        [str(BIN), "--storage", str(STORE), *args], cwd=WORK / "A",
                        env=env, capture_output=True, text=True, timeout=30,
                    )
                recorded = call("record", name, "--", "/synthetic/target")
                self.assertEqual(recorded.returncode, 0, recorded.stderr)
                shown = call("show", name, "--json")
                self.assertEqual(shown.returncode, 0, shown.stderr)
                run = json.loads(shown.stdout)
                metadata = run["metadata"]
                failed = result != "0"
                self.assertEqual(metadata["state"], "completed")
                self.assertEqual(metadata["completeness"], "complete_for_supported_events" if failed else "partial")
                self.assertEqual(len(metadata["warnings"]), 0 if failed else 1)
                if not failed:
                    self.assertTrue(metadata["warnings"][0].startswith("raw/trace.log:4: fs.context:"))
                self.assertEqual((STORE / "runs" / metadata["id"] / "raw/trace.log").read_text(), raw)
                for line, uncertain, cwd in [
                    (6, not failed and not split, "/B" if failed else str(WORK / "A") if split else None),
                    (7, not failed, "/B" if failed else None),
                    (8, False, str(WORK / "A")),
                ]:
                    event = next(e for e in run["events"] if e["evidence"]["line"] == line)
                    self.assertEqual(event["cwd_before"], cwd)
                    self.assertEqual(event["context_uncertain"], uncertain)
                    self.assertEqual(event["schema_version"], 2)
                    self.assertEqual(event["evidence"], {"file": "raw/trace.log", "line": line, "end_line": line})
                data, text = call("diff", name, name, "--json"), call("diff", name, name)
                self.assertEqual((data.returncode, text.returncode), (0, 0) if failed else (3, 3))
                report = json.loads(data.stdout)
                self.assertFalse(report["observed_differences"])
                self.assertEqual(report["reliability"]["left"], {
                    "capture": "complete" if failed else "incomplete", "storage": "verified",
                })
                self.assertEqual("RELIABILITY NOT ESTABLISHED" in text.stdout, not failed)

    def test_pid_lifetime_boundaries_keep_pending_evidence_and_source_uncertainty(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        cases = [
            ("exit-reuse", [
                '1 clone(flags=CLONE_FS|SIGCHLD) = 2',
                '2 chdir("/old" <unfinished ...>',
                '2 +++ exited with 0 +++',
                '1 clone(flags=CLONE_FS|SIGCHLD) = 2',
                '2 <... chdir resumed>) = 0',
            ], str(WORK / "A"), False, "unfinished call", False),
            ("prefix-before-spawn", [
                '2 chdir("/old" <unfinished ...>',
                '1 clone(flags=CLONE_FS|SIGCHLD <unfinished ...>',
                '1 <... clone resumed>) = 2',
                '2 <... chdir resumed>) = 0',
            ], str(WORK / "A"), False, "predates spawn entry", False),
            ("prefix-after-entry", [
                '1 clone(flags=CLONE_FS|SIGCHLD <unfinished ...>',
                '2 chdir("/B" <unfinished ...>',
                '1 <... clone resumed>) = 2',
                '2 <... chdir resumed>) = 0',
            ], "/B", False, None, True),
            ("shared-collision", [
                '1 fork() = 2',
                '1 clone(flags=CLONE_FS|SIGCHLD) = 2',
                '2 chdir("/B") = 0',
            ], None, True, "LiveCollision", True),
            ("private-collision", [
                '1 fork() = 2',
                '1 fork() = 2',
                '2 chdir("/B") = 0',
            ], str(WORK / "A"), False, "LiveCollision", True),
        ]
        for contract in (None, "trace-v1"):
            for case, observations, cwd, uncertain, warning, has_change in cases:
                name = f"lifetime-{contract or 'default'}-{case}"
                with self.subTest(name=name):
                    raw = "\n".join([
                        '1 execve("/synthetic/target", [], 0) = 0', *observations,
                        '1 open("/parent", O_RDONLY) = 3',
                        '2 +++ exited with 0 +++', '1 +++ exited with 0 +++', "",
                    ])
                    trace = WORK / f"{name}.trace"
                    trace.write_text(raw)
                    env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}
                    def call(*args):
                        return subprocess.run([str(BIN), "--storage", str(STORE), *args],
                                              cwd=WORK / "A", env=env, capture_output=True,
                                              text=True, timeout=30)
                    recording = ["record", name]
                    if contract:
                        recording += ["--evidence-contract", contract]
                    recorded = call(*recording, "--", "/synthetic/target")
                    self.assertEqual(recorded.returncode, 0, recorded.stderr)
                    shown = call("show", name, "--json")
                    self.assertEqual(shown.returncode, 0, shown.stderr)
                    run = json.loads(shown.stdout)
                    metadata = run["metadata"]
                    self.assertEqual(metadata["state"], "completed")
                    self.assertEqual(metadata["exit_code"], 0)
                    self.assertEqual(metadata["schema_version"], 4 if contract else 3)
                    self.assertEqual(metadata["completeness"], "partial" if warning else "complete_for_supported_events")
                    if warning:
                        self.assertTrue(any(warning in w for w in metadata["warnings"]), metadata["warnings"])
                    else:
                        self.assertFalse(metadata["warnings"])
                    changes = [e for e in run["events"] if e["operation"] == "chdir"]
                    self.assertEqual(len(changes), 1 if has_change else 0)
                    if case == "prefix-after-entry":
                        self.assertEqual(changes[0]["evidence"], {"file": "raw/trace.log", "line": 3, "end_line": 5})
                    probe_line = len(observations) + 2
                    probe = next(e for e in run["events"] if e["evidence"]["line"] == probe_line)
                    self.assertEqual(probe["cwd_before"], cwd)
                    self.assertEqual(probe["context_uncertain"], uncertain)
                    self.assertEqual(probe["schema_version"], 3 if contract else 2)
                    self.assertEqual(probe["evidence"], {"file": "raw/trace.log", "line": probe_line, "end_line": probe_line})
                    if contract and case == "exit-reuse":
                        spawns = [e for e in run["events"] if e["kind"] == "spawn"]
                        self.assertEqual([e["child_generation"] for e in spawns], [1, 2])
                        exits = [e for e in run["events"] if e["kind"] == "exit" and e["pid"] == "2"]
                        self.assertEqual([e["task_generation"] for e in exits], [1, 2])
                    self.assertEqual((STORE / "runs" / metadata["id"] / "raw/trace.log").read_text(), raw)
                    data, text = call("diff", name, name, "--json"), call("diff", name, name)
                    self.assertEqual((data.returncode, text.returncode), (3, 3) if warning else (0, 0))
                    report = json.loads(data.stdout)
                    self.assertFalse(report["observed_differences"])
                    expected_reliability = {
                        "capture": "incomplete" if warning else "complete", "storage": "verified",
                    }
                    if contract:
                        expected_reliability |= {"raw": "verified", "provenance": "verified"}
                    self.assertEqual(report["reliability"]["left"], expected_reliability)
                    self.assertEqual("RELIABILITY NOT ESTABLISHED" in text.stdout, warning is not None)

    def test_pending_private_fork_snapshot_is_evidence_bounded(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        cases = [
            (
                "fork-snapshot-overlap",
                '2 chdir("/B") = 0',
                r'3 open("payload", O_RDONLY) = 4<\x2f\x42\x2f\x70>',
                "partial",
                3,
            ),
            (
                "fork-snapshot-failed-transition",
                '2 chdir("/B") = -1 ENOENT (missing)',
                '3 open("/child", O_RDONLY) = 3',
                "complete_for_supported_events",
                0,
            ),
        ]
        for name, transition, child_access, completeness, diff_rc in cases:
            with self.subTest(name=name):
                trace = WORK / f"{name}.trace"
                trace.write_text("\n".join([
                    '1 execve("/tool", [], 0) = 0',
                    '1 clone(flags=CLONE_FS|SIGCHLD) = 2',
                    '1 fork( <unfinished ...>',
                    transition,
                    '1 <... fork resumed>) = 3',
                    child_access,
                    '3 +++ exited with 0 +++',
                    '2 +++ exited with 0 +++',
                    '1 +++ exited with 0 +++',
                    "",
                ]))
                env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}
                recorded = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "record", name, "--", "/synthetic/target"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(recorded.returncode, 0, recorded.stderr)
                shown = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "show", name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(shown.returncode, 0, shown.stderr)
                run = json.loads(shown.stdout)
                self.assertEqual(run["metadata"]["state"], "completed")
                self.assertEqual(run["metadata"]["completeness"], completeness)
                spawn = next(event for event in run["events"]
                             if event["evidence"]["line"] == 3)
                transition_event = next(event for event in run["events"]
                                        if event["evidence"]["line"] == 4)
                child = next(event for event in run["events"]
                             if event["evidence"]["line"] == 6)
                self.assertEqual(spawn["evidence"]["end_line"], 5)
                if diff_rc == 3:
                    warning = (
                        "raw/trace.log:4: fs.context: directory change overlapped an "
                        "unresolved private clone snapshot; affected relative path context is incomplete"
                    )
                    self.assertEqual(run["metadata"]["warnings"], [warning])
                    self.assertTrue(spawn["context_uncertain"])
                    self.assertTrue(transition_event["context_uncertain"])
                    self.assertTrue(child["context_uncertain"])
                    self.assertIsNone(child["cwd_before"])
                else:
                    self.assertFalse(run["metadata"]["warnings"])
                    self.assertFalse(spawn["context_uncertain"])
                    self.assertFalse(transition_event["context_uncertain"])
                    self.assertFalse(child["context_uncertain"])
                    self.assertEqual(child["cwd_before"], str(WORK / "A"))
                report = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "diff", name, name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                text_report = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "diff", name, name],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual((report.returncode, text_report.returncode), (diff_rc, diff_rc))
                comparison = json.loads(report.stdout)
                self.assertFalse(comparison["observed_differences"])
                expected_capture = "incomplete" if diff_rc == 3 else "complete"
                self.assertEqual(comparison["reliability"]["left"], {
                    "capture": expected_capture, "storage": "verified",
                })

    def test_mount_operation_matrix_and_selector_contract(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        operations = [
            ("mount", 'mount("none", "/mnt", "tmpfs", 0, NULL)'),
            ("umount2", 'umount2("/mnt", MNT_DETACH)'),
            ("move_mount", 'move_mount(AT_FDCWD, "/old", AT_FDCWD, "/new", 0)'),
            (
                "mount_setattr",
                'mount_setattr(AT_FDCWD, "/mnt", 0, {attr_set=MOUNT_ATTR_RDONLY}, 32)',
            ),
        ]
        for syscall, operation in operations:
            for successful in [True, False]:
                name = f"{syscall}-{'success' if successful else 'failed'}"
                with self.subTest(name=name):
                    result = "0" if successful else "-1 EPERM (denied)"
                    trace = WORK / f"{name}.trace"
                    trace.write_text("\n".join([
                        '1 execve("/tool", [], 0) = 0',
                        '1 fork() = 3',
                        '1 clone(flags=CLONE_FS|SIGCHLD) = 2',
                        f"2 {operation} = {result}",
                        '1 open("/parent", O_RDONLY) = 3',
                        '2 open("/member", O_RDONLY) = 3',
                        '3 open("/private", O_RDONLY) = 3',
                        '3 +++ exited with 0 +++',
                        '2 +++ exited with 0 +++',
                        '1 +++ exited with 0 +++',
                        "",
                    ]))
                    env = os.environ | {
                        "RUNDELTA_STRACE": str(backend),
                        "FS_TRACE": str(trace),
                    }
                    recorded = subprocess.run(
                        [str(BIN), "--storage", str(STORE), "record", name, "--", "/synthetic/target"],
                        cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                    )
                    self.assertEqual(recorded.returncode, 0, recorded.stderr)
                    shown = subprocess.run(
                        [str(BIN), "--storage", str(STORE), "show", name, "--json"],
                        cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                    )
                    self.assertEqual(shown.returncode, 0, shown.stderr)
                    run = json.loads(shown.stdout)
                    expected_completeness = "partial" if successful else "complete_for_supported_events"
                    self.assertEqual(run["metadata"]["completeness"], expected_completeness)
                    transition = next(event for event in run["events"]
                                      if event["operation"] == syscall)
                    self.assertEqual(transition["evidence"], {
                        "file": "raw/trace.log", "line": 4, "end_line": 4,
                    })
                    self.assertEqual(transition["outcome"], "success" if successful else "EPERM")
                    affected = [event for event in run["events"]
                                if 4 <= event["evidence"]["line"] <= 7]
                    self.assertTrue(affected)
                    self.assertTrue(all(event["context_uncertain"] == successful
                                        for event in affected), affected)
                    if successful:
                        self.assertEqual(len(run["metadata"]["warnings"]), 1)
                        self.assertTrue(run["metadata"]["warnings"][0].startswith(
                            "raw/trace.log:4: fs.context: mount namespace transition"
                        ))
                    else:
                        self.assertFalse(run["metadata"]["warnings"])
                    report = subprocess.run(
                        [str(BIN), "--storage", str(STORE), "diff", name, name, "--json"],
                        cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                    )
                    self.assertEqual(report.returncode, 3 if successful else 0)
                    comparison = json.loads(report.stdout)
                    self.assertFalse(comparison["observed_differences"])
                    self.assertEqual(comparison["reliability"]["left"]["capture"],
                                     "incomplete" if successful else "complete")

    def test_unsupported_required_selector_fails_before_target_execution(self):
        backend = WORK / "unsupported-selector-backend"
        target_marker = WORK / "unsupported-selector-target-ran"
        backend.write_text("""#!/usr/bin/env python3
import os
import subprocess
import sys
if "--version" in sys.argv[1:]:
    if "-e" in sys.argv and "mount_setattr" in sys.argv[sys.argv.index("-e") + 1]:
        sys.stderr.write("invalid system call 'mount_setattr'\\n")
        sys.exit(64)
    print("selector contract backend")
    sys.exit(0)
target = sys.argv[sys.argv.index("--") + 1:]
sys.exit(subprocess.run(target).returncode)
""")
        backend.chmod(0o700)
        env = os.environ | {"RUNDELTA_STRACE": str(backend)}
        recorded = subprocess.run(
            [
                str(BIN), "--storage", str(STORE), "record", "unsupported-selector",
                "--", "/usr/bin/touch", str(target_marker),
            ],
            cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(recorded.returncode, 2, recorded.stderr)
        # A nonzero probe is an observed fact, not proof of its cause. Preserve
        # the backend's bounded diagnostic without having RunDelta infer that
        # every nonzero exit necessarily means selector incompatibility.
        self.assertIn("strace version probe exited nonzero (64)", recorded.stderr)
        self.assertIn("invalid system call 'mount_setattr'", recorded.stderr)
        self.assertFalse(target_marker.exists(), "target ran after selector preflight failed")

    def test_untrusted_task_generations_are_partial_and_self_diff_three(self):
        backend = ROOT / "tests/fixtures/fs_synthetic_backend.py"
        cases = [
            (
                "unintroduced-generation",
                [
                    '20 execve("/synthetic/target", [], 0) = 0',
                    '21 open("/unintroduced", O_RDONLY) = 3',
                    '21 +++ exited with 0 +++',
                    '20 +++ exited with 0 +++',
                ],
                2,
                "Unintroduced",
            ),
            (
                "reappeared-generation",
                [
                    '20 execve("/synthetic/target", [], 0) = 0',
                    '20 fork() = 21',
                    '21 +++ exited with 0 +++',
                    '21 open("/reappeared", O_RDONLY) = 3',
                    '21 +++ exited with 0 +++',
                    '20 +++ exited with 0 +++',
                ],
                4,
                "Reappeared",
            ),
            (
                "live-collision-generation",
                [
                    '20 execve("/synthetic/target", [], 0) = 0',
                    '20 fork() = 21',
                    '20 fork() = 21',
                    '21 open("/collision", O_RDONLY) = 3',
                    '21 +++ exited with 0 +++',
                    '20 +++ exited with 0 +++',
                ],
                4,
                "LiveCollision",
            ),
        ]
        for name, lines, uncertain_line, origin in cases:
            with self.subTest(name=name):
                trace = WORK / f"{name}.trace"
                trace.write_text("\n".join(lines) + "\n")
                env = os.environ | {"RUNDELTA_STRACE": str(backend), "FS_TRACE": str(trace)}

                recorded = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "record", name, "--", "/synthetic/target"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(recorded.returncode, 0, recorded.stderr)
                shown = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "show", name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(shown.returncode, 0, shown.stderr)
                run = json.loads(shown.stdout)
                self.assertEqual(run["metadata"]["completeness"], "partial")
                identity = [warning for warning in run["metadata"]["warnings"]
                            if "task.identity:" in warning]
                self.assertEqual(len(identity), 1, run["metadata"]["warnings"])
                self.assertIn(origin, identity[0])
                event = next(event for event in run["events"]
                             if event["evidence"]["line"] == uncertain_line)
                self.assertTrue(event["context_uncertain"], event)

                report = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "diff", name, name, "--json"],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                text_report = subprocess.run(
                    [str(BIN), "--storage", str(STORE), "diff", name, name],
                    cwd=WORK / "A", env=env, capture_output=True, text=True, timeout=30,
                )
                self.assertEqual((report.returncode, text_report.returncode), (3, 3))
                data = json.loads(report.stdout)
                self.assertFalse(data["observed_differences"])
                self.assertEqual(data["reliability"]["left"], {
                    "capture": "incomplete", "storage": "verified",
                })
                self.assertIn("RELIABILITY NOT ESTABLISHED", text_report.stdout)


if __name__ == "__main__":
    try:
        main()
    finally:
        print("ARTIFACTS:", WORK)
