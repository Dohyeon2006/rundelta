#!/usr/bin/env python3
"""Deterministic tests for the alternate-index/tree release closure checker."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_release_closure import (
    CONTRACT_ASSETS,
    CONTRACT_TESTS,
    Candidate,
    NEW_RELEASE_ASSETS,
    REVIEWED_FIXTURE_ADDITIONS,
    REVIEWED_EVIDENCE_ADDITIONS,
    validate_release_closure,
)


class FixtureRepository:
    """Small Git repository with an old HEAD and a complete new worktree."""

    def __init__(self, *, historical_evidence_present: bool = True) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="release-closure-test-")
        self.root = Path(self.temporary.name)
        self.indexes = self.root / "candidate-indexes"
        self.next_index = 0
        self.created_paths: set[str] = set()
        self.git("init", "-q")
        self.git("config", "user.name", "Release Closure Test")
        self.git("config", "user.email", "release-closure@example.invalid")

        self.write("README.md", b"baseline README\n")
        if historical_evidence_present:
            self.write(
                "docs/performance/a3-completion-check/README.md",
                b"baseline A3 evidence\n",
            )
            self.write(
                "docs/external-alpha/attribution/probe.c",
                b"/* baseline attribution probe */\n",
            )
        for path in (
            "examples/run.py",
            "tests/completion_equivalence.py",
            "tests/cwd_uncertainty.py",
            "tests/fs_context.py",
            "tests/reliability.py",
            "tests/storage_cli.py",
        ):
            self.write(path, f"# baseline {path}\n".encode())
        self.write("tests/acceptance.rs", b"// old acceptance registrations\n")
        self.git("add", "--", *sorted(self.created_paths))
        self.git("commit", "-q", "-m", "baseline")

        for path in sorted(NEW_RELEASE_ASSETS):
            if path in REVIEWED_FIXTURE_ADDITIONS or (
                path in REVIEWED_EVIDENCE_ADDITIONS and not historical_evidence_present
            ):
                self.write(path, (Path(__file__).resolve().parents[1] / path).read_bytes())
            elif not (self.root / path).exists():
                self.write(path, f"asset:{path}\n".encode())
        self.write(
            "tests/acceptance.rs",
            b'.arg("examples/run.py")\n'
            b'.arg("tests/reliability.py")\n'
            b'.arg("tests/runtime_boundary.py")\n'
            b'.arg("tests/storage_cli.py")\n'
            b'.arg("tests/cwd_uncertainty.py")\n'
            b'.arg("tests/completion_equivalence.py")\n'
            b'.arg("tests/fs_context.py")\n'
            b'.arg("tests/mount_namespace.py")\n',
        )
        self.write(
            "tests/runtime_boundary.py",
            b"FIXTURES = [\n"
            b'    "tests/fixtures/runtime_boundary_probe.c",\n'
            b'    "tests/fixtures/pthread_eagain.c",\n'
            b'    "tests/fixtures/prctl_fail.c",\n'
            b'    "tests/fixtures/version_backend.py",\n'
            b"]\n",
        )
        self.write("tests/mount_namespace.py", b"MOUNT = True\n")
        self.write(
            "tests/full_gate.py",
            b'CLOSURE = "tests/check_release_closure.py"\n'
            b'CANDIDATE = "--candidate-index"\n',
        )
        contract = "\n".join(f"#[test]\nfn {name}() {{}}" for name in CONTRACT_TESTS)
        contract += "\n"
        for path in CONTRACT_ASSETS:
            if "/golden/" in path:
                relative = path.removeprefix("tests/")
                contract += f'const _: &[u8] = include_bytes!("{relative}");\n'
        self.write("tests/contract_baseline.rs", contract.encode())
        self.write(
            "docs/REFACTOR_STATUS.md",
            b"REFACTOR_DECISIONS.md\n"
            b"performance/rf600-streaming/README.md\n"
            b"tests/full_gate.py\n",
        )

    def close(self) -> None:
        self.temporary.cleanup()

    def write(self, path: str, content: bytes) -> None:
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)
        self.created_paths.add(path)

    def git(
        self,
        *arguments: str,
        index: Path | None = None,
        capture: bool = False,
    ) -> str:
        environment = os.environ.copy()
        if index is not None:
            environment["GIT_INDEX_FILE"] = str(index)
        result = subprocess.run(
            ["git", "-C", str(self.root), *arguments],
            env=environment,
            stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            self.fail_git(arguments, result)
        return result.stdout.strip() if capture else ""

    @staticmethod
    def fail_git(arguments: tuple[str, ...], result: subprocess.CompletedProcess[str]) -> None:
        raise AssertionError(
            f"git {arguments!r} failed with {result.returncode}: {result.stderr}"
        )

    def candidate_index(
        self,
        *,
        omit: set[str] | None = None,
        add_private: bool = False,
        old_only: bool = False,
    ) -> Path:
        self.indexes.mkdir(exist_ok=True)
        index = self.indexes / f"index-{self.next_index}"
        self.next_index += 1
        self.git("read-tree", "HEAD", index=index)
        paths = {"tests/acceptance.rs"}
        if not old_only:
            paths.update(NEW_RELEASE_ASSETS)
        paths.difference_update(omit or set())
        if add_private:
            self.write(".idea/workspace.xml", b"private IDE state\n")
            paths.add(".idea/workspace.xml")
        self.git("add", "--", *sorted(paths), index=index)
        for path in sorted((omit or set()) & NEW_RELEASE_ASSETS):
            self.git("update-index", "--force-remove", path, index=index)
        return index

    def errors(self, index: Path) -> list[str]:
        return validate_release_closure(
            self.root, Candidate.from_index(self.root, index)
        )


class ReleaseClosureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = FixtureRepository()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_complete_alternate_index_and_its_tree_pass(self) -> None:
        index = self.fixture.candidate_index()
        self.assertEqual(self.fixture.errors(index), [])
        tree = self.fixture.git("write-tree", index=index, capture=True)
        self.assertEqual(
            validate_release_closure(
                self.fixture.root,
                Candidate.from_tree(self.fixture.root, tree),
            ),
            [],
        )

    def test_old_tracked_only_candidate_fails_closed(self) -> None:
        errors = self.fixture.errors(self.fixture.candidate_index(old_only=True))
        self.assertIn("missing from candidate: tests/runtime_boundary.py", errors)
        self.assertIn("missing from candidate: tests/contract_baseline.rs", errors)
        self.assertIn("missing from candidate: tests/full_gate.py", errors)

    def test_missing_acceptance_script_and_fixture_are_distinct_failures(self) -> None:
        script_errors = self.fixture.errors(
            self.fixture.candidate_index(omit={"tests/runtime_boundary.py"})
        )
        self.assertIn("missing from candidate: tests/runtime_boundary.py", script_errors)

        fixture_errors = self.fixture.errors(
            self.fixture.candidate_index(
                omit={"tests/fixtures/runtime_boundary_probe.c"}
            )
        )
        self.assertIn(
            "missing from candidate: tests/fixtures/runtime_boundary_probe.c",
            fixture_errors,
        )

    def test_missing_contract_asset_is_rejected(self) -> None:
        missing = "tests/fixtures/contract_baseline/golden/diff_3.json"
        errors = self.fixture.errors(self.fixture.candidate_index(omit={missing}))
        self.assertIn(f"missing from candidate: {missing}", errors)

    def test_each_cwd_tree_warning_contract_asset_is_required(self) -> None:
        for path in (
            "tests/check_cwd_tree_warning_contract.py",
            "tests/cwd_tree_warning_contract.py",
            "tests/fixtures/cwd_tree_warning_contract_v1.json",
            "tests/test_cwd_tree_warning_contract.py",
        ):
            with self.subTest(path=path):
                errors = self.fixture.errors(self.fixture.candidate_index(omit={path}))
                self.assertIn(f"missing from candidate: {path}", errors)

    def test_exact_csv_attributes_asset_is_required(self) -> None:
        path = ".gitattributes"
        errors = self.fixture.errors(self.fixture.candidate_index(omit={path}))
        self.assertIn(f"missing from candidate: {path}", errors)

    def test_every_public_export_addition_is_required(self) -> None:
        fixture = FixtureRepository(historical_evidence_present=False)
        self.addCleanup(fixture.close)
        for path in sorted(NEW_RELEASE_ASSETS):
            with self.subTest(path=path):
                errors = fixture.errors(fixture.candidate_index(omit={path}))
                self.assertIn(f"missing from candidate: {path}", errors)

    def test_reviewed_fixture_additions_are_pinned_in_candidate_blobs(self) -> None:
        index = self.fixture.candidate_index()
        self.assertEqual(self.fixture.errors(index), [])
        for path in REVIEWED_FIXTURE_ADDITIONS:
            with self.subTest(path=path):
                original = (self.fixture.root / path).read_bytes()
                self.fixture.write(path, original + b" ")
                changed = self.fixture.candidate_index()
                self.assertIn(f"reviewed fixture addition hash mismatch: {path}",
                              self.fixture.errors(changed))
                self.fixture.write(path, original)

    def test_existing_reviewed_fixture_changes_and_deletions_are_rejected(self) -> None:
        index = self.fixture.candidate_index()
        self.fixture.git("commit", "-q", "-m", "reviewed fixtures", index=index)
        for path in REVIEWED_FIXTURE_ADDITIONS:
            with self.subTest(path=path):
                original = (self.fixture.root / path).read_bytes()
                self.fixture.write(path, original + b" ")
                changed = self.fixture.candidate_index()
                self.assertIn(f"candidate changes excluded existing path: {path}",
                              self.fixture.errors(changed))
                self.fixture.write(path, original)
                missing = self.fixture.candidate_index()
                self.fixture.git("update-index", "--force-remove", path, index=missing)
                self.assertIn(f"candidate changes excluded existing path: {path}",
                              self.fixture.errors(missing))

    def test_reviewed_historical_evidence_first_additions_are_exact(self) -> None:
        fixture = FixtureRepository(historical_evidence_present=False)
        self.addCleanup(fixture.close)
        index = fixture.candidate_index()
        self.assertEqual(fixture.errors(index), [])
        tree = fixture.git("write-tree", index=index, capture=True)
        self.assertEqual(validate_release_closure(
            fixture.root, Candidate.from_tree(fixture.root, tree)), [])
        for path in REVIEWED_EVIDENCE_ADDITIONS:
            with self.subTest(path=path):
                original = (fixture.root / path).read_bytes()
                fixture.write(path, original + b" ")
                changed = fixture.candidate_index()
                self.assertIn(f"reviewed evidence addition hash mismatch: {path}",
                              fixture.errors(changed))
                changed_tree = fixture.git("write-tree", index=changed, capture=True)
                self.assertIn(f"reviewed evidence addition hash mismatch: {path}",
                              validate_release_closure(fixture.root,
                                  Candidate.from_tree(fixture.root, changed_tree)))
                fixture.write(path, original)

    def test_existing_historical_evidence_changes_and_deletions_still_fail(self) -> None:
        for path in REVIEWED_EVIDENCE_ADDITIONS:
            with self.subTest(path=path):
                original = (self.fixture.root / path).read_bytes()
                self.fixture.write(path, original + b" ")
                changed = self.fixture.candidate_index()
                self.assertIn(f"candidate changes excluded existing path: {path}",
                              self.fixture.errors(changed))
                self.fixture.write(path, original)
                missing = self.fixture.candidate_index()
                self.fixture.git("update-index", "--force-remove", path, index=missing)
                self.assertIn(f"candidate changes excluded existing path: {path}",
                              self.fixture.errors(missing))

    def test_stale_staged_blob_is_rejected(self) -> None:
        index = self.fixture.candidate_index()
        self.fixture.write("tests/runtime_boundary.py", b"changed after staging\n")
        self.assertIn(
            "candidate blob differs from worktree bytes: tests/runtime_boundary.py",
            self.fixture.errors(index),
        )

    def test_private_addition_and_historical_evidence_change_are_rejected(self) -> None:
        private_errors = self.fixture.errors(
            self.fixture.candidate_index(add_private=True)
        )
        self.assertIn(
            "candidate changes forbidden private path: .idea/workspace.xml",
            private_errors,
        )
        self.assertIn(
            "candidate adds path outside the release asset allowlist: .idea/workspace.xml",
            private_errors,
        )

        probe = "docs/external-alpha/attribution/probe.c"
        self.fixture.write(probe, b"changed historical probe\n")
        index = self.fixture.candidate_index()
        self.fixture.git("add", "--", probe, index=index)
        self.assertIn(
            f"candidate changes excluded existing path: {probe}",
            self.fixture.errors(index),
        )

    def test_reviewed_readme_must_match_the_explicit_candidate(self) -> None:
        self.fixture.write("README.md", b"reviewed owner contract\n")
        index = self.fixture.candidate_index()
        self.fixture.git("add", "--", "README.md", index=index)
        self.assertEqual(self.fixture.errors(index), [])
        self.fixture.write("README.md", b"later unbound README change\n")
        self.assertIn(
            "candidate blob differs from worktree bytes: README.md",
            self.fixture.errors(index),
        )
        missing = self.fixture.candidate_index()
        self.fixture.git("update-index", "--force-remove", "README.md", index=missing)
        self.assertIn("missing from candidate: README.md", self.fixture.errors(missing))

    def test_owner_source_and_private_probe_assets_are_required(self) -> None:
        for path in (
            "src/capture/owner.rs", "tests/capture_owner_support.py",
            "tests/probes/capture_caught_control.c", "tests/probes/capture_chld_target.c",
            "tests/probes/capture_host_hooks.c", "tests/probes/capture_request_fault.c",
            "tests/probes/capture_syscall.c",
        ):
            with self.subTest(path=path):
                errors = self.fixture.errors(self.fixture.candidate_index(omit={path}))
                self.assertIn(f"missing from candidate: {path}", errors)

    def test_policy_literals_do_not_pull_excluded_worktree_edits_into_closure(self) -> None:
        self.fixture.write(
            "tests/check_release_closure.py",
            b'IMMUTABLE = "docs/performance/a3-completion-check/README.md"\n',
        )
        index = self.fixture.candidate_index()
        self.fixture.write(
            "docs/performance/a3-completion-check/README.md",
            b"uncommitted user evidence edit\n",
        )
        self.assertEqual(self.fixture.errors(index), [])
        self.fixture.git("add", "--", "docs/performance/a3-completion-check/README.md", index=index)
        self.assertIn(
            "candidate changes excluded existing path: docs/performance/a3-completion-check/README.md",
            self.fixture.errors(index),
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
