#!/usr/bin/env python3
"""Verify that one explicit Git candidate contains the release asset closure.

The checker reads either an alternate index or an existing Git tree.  It never
updates the repository index, creates a commit, or substitutes working-tree
files for candidate blobs.  Required release assets must nevertheless match
the current working-tree bytes so a stale staged copy cannot pass.
"""

from __future__ import annotations

import argparse
import hashlib
import ast
from dataclasses import dataclass
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
from typing import Iterable


ROOT = Path(__file__).resolve().parents[1]

CONTRACT_ASSETS = (
    "tests/fixtures/contract_baseline/README.md",
    "tests/fixtures/contract_baseline/baseline_manifest.json",
    "tests/fixtures/contract_baseline/golden/diff_0.json",
    "tests/fixtures/contract_baseline/golden/diff_0.txt",
    "tests/fixtures/contract_baseline/golden/diff_1.json",
    "tests/fixtures/contract_baseline/golden/diff_1.txt",
    "tests/fixtures/contract_baseline/golden/diff_2.json",
    "tests/fixtures/contract_baseline/golden/diff_2.txt",
    "tests/fixtures/contract_baseline/golden/diff_3.json",
    "tests/fixtures/contract_baseline/golden/diff_3.txt",
    "tests/fixtures/contract_baseline/golden/list.json",
    "tests/fixtures/contract_baseline/golden/list.txt",
    "tests/fixtures/contract_baseline/golden/show.json",
    "tests/fixtures/contract_baseline/golden/show.txt",
    "tests/fixtures/contract_baseline/storage/runs/abc001-101/events.jsonl",
    "tests/fixtures/contract_baseline/storage/runs/abc001-101/finalized.json",
    "tests/fixtures/contract_baseline/storage/runs/abc001-101/metadata.json",
    "tests/fixtures/contract_baseline/storage/runs/abc001-101/raw/trace.log",
    "tests/fixtures/contract_baseline/storage/runs/abc002-102/events.jsonl",
    "tests/fixtures/contract_baseline/storage/runs/abc002-102/finalized.json",
    "tests/fixtures/contract_baseline/storage/runs/abc002-102/metadata.json",
    "tests/fixtures/contract_baseline/storage/runs/abc002-102/raw/trace.log",
    "tests/fixtures/contract_baseline/storage/runs/abc101-201/events.jsonl",
    "tests/fixtures/contract_baseline/storage/runs/abc101-201/metadata.json",
    "tests/fixtures/contract_baseline/storage/runs/abc101-201/raw/trace.log",
    "tests/fixtures/contract_baseline/storage/runs/abc102-202/events.jsonl",
    "tests/fixtures/contract_baseline/storage/runs/abc102-202/metadata.json",
    "tests/fixtures/contract_baseline/storage/runs/abc102-202/raw/trace.log",
)

# Explicit public-export additions relative to reviewed public main.
NEW_RELEASE_ASSETS = frozenset(
    {
        ".gitattributes",
        'docs/ACCEPTANCE-before-reliability.md',
        'docs/ACCEPTANCE.md',
        'docs/ALPHA_READINESS.md',
        'docs/ARCHITECTURE_REVIEW.md',
        'docs/CRATE_README.md',
        'docs/FIELD_AUDIT_LAYERS.md',
        'docs/FIELD_CASES.md',
        'docs/FIELD_CASES_RESULTS.json',
        'docs/FS_CONTEXT_MODEL.md',
        'docs/HOLDOUT_EVALUATION.md',
        'docs/HOLDOUT_PLAN.md',
        'docs/HOLDOUT_RESULTS.json',
        'docs/INCOMPLETE_CAPTURE.md',
        'docs/INDEX.md',
        'docs/RANKING_REPAIR.md',
        'docs/RANKING_REPAIR_RESULTS.json',
        'docs/REFACTOR_DECISIONS.md',
        'docs/REFACTOR_STATUS.md',
        'docs/REGRESSION_SET.json',
        'docs/REGRESSION_SET.md',
        'docs/REGRESSION_SET_PUBLIC.json',
        'docs/external-alpha/CHECKS.md',
        'docs/external-alpha/README.md',
        'docs/external-alpha/REPLAY.json',
        'docs/external-alpha/REPRODUCE.md',
        'docs/external-alpha/attribution/README.md',
        'docs/external-alpha/attribution/RESULTS.json',
        'docs/external-alpha/attribution/original-materials.json',
        'docs/external-alpha/attribution/probe.c',
        'docs/external-alpha/attribution/probe.py',
        'docs/external-alpha/attribution/verify.py',
        'docs/external-alpha/batch-2/CHECKS.md',
        'docs/external-alpha/batch-2/README.md',
        'docs/external-alpha/batch-2/SCREENING.md',
        'docs/external-alpha/batch-2/analyze.py',
        'docs/external-alpha/batch-2/bracket-lib-376/CASE.md',
        'docs/external-alpha/batch-2/bracket-lib-376/RESULTS.md',
        'docs/external-alpha/batch-2/bracket-lib-376/evidence.json',
        'docs/external-alpha/batch-2/bracket-lib-376/summary.json',
        'docs/external-alpha/batch-2/original-materials.json',
        'docs/external-alpha/batch-2/pip-12996/CASE.md',
        'docs/external-alpha/batch-2/pip-12996/RESULTS.md',
        'docs/external-alpha/batch-2/pip-12996/evidence.json',
        'docs/external-alpha/batch-2/pip-12996/summary.json',
        'docs/external-alpha/batch-2/provenance.json',
        'docs/external-alpha/batch-2/reproduce.py',
        'docs/external-alpha/batch-2/test_redaction.py',
        'docs/external-alpha/batch-2/verify.py',
        'docs/external-alpha/clone-fs-attribution/EVIDENCE.json',
        'docs/external-alpha/clone-fs-attribution/PLAN.md',
        'docs/external-alpha/clone-fs-attribution/README.md',
        'docs/external-alpha/clone-fs-attribution/RESULTS.json',
        'docs/external-alpha/clone-fs-attribution/collect.py',
        'docs/external-alpha/clone-fs-attribution/original-materials.json',
        'docs/external-alpha/clone-fs-attribution/probe.c',
        'docs/external-alpha/clone-fs-attribution/verify.py',
        'docs/external-alpha/meson-16136/CASE.md',
        'docs/external-alpha/meson-16136/RESULTS.md',
        'docs/external-alpha/meson-16136/evidence-excerpts.json',
        'docs/external-alpha/meson-16136/preregistration.sha256',
        'docs/external-alpha/meson-16136/summary.json',
        'docs/external-alpha/preservation.json',
        'docs/external-alpha/provenance.json',
        'docs/external-alpha/pybind11-3445/CASE.md',
        'docs/external-alpha/pybind11-3445/RESULTS.md',
        'docs/external-alpha/pybind11-3445/evidence-excerpts.json',
        'docs/external-alpha/pybind11-3445/preregistration.sha256',
        'docs/external-alpha/pybind11-3445/summary.json',
        'docs/external-alpha/reproduce.py',
        'docs/external-alpha/spades-1339/CASE.md',
        'docs/external-alpha/spades-1339/RESULTS.md',
        'docs/external-alpha/spades-1339/RETRY-EXACT.md',
        'docs/external-alpha/spades-1339/RETRY.md',
        'docs/external-alpha/spades-1339/attempts.json',
        'docs/external-alpha/spades-1339/evidence-excerpts.json',
        'docs/external-alpha/spades-1339/preregistration.sha256',
        'docs/external-alpha/spades-1339/summary.json',
        'docs/external-alpha/verify.py',
        'docs/external-cases/pybind11-5626/FROZEN.md',
        'docs/external-cases/pybind11-5626/NOISE_ANALYSIS.md',
        'docs/external-cases/pybind11-5626/README.md',
        'docs/external-cases/pybind11-5626/REPORT.md',
        'docs/external-cases/pybind11-5626/evidence.json',
        'docs/external-cases/pybind11-5626/freeze-manifest.json',
        'docs/external-cases/pybind11-5626/noise-summary.json',
        'docs/external-cases/pybind11-5626/parent-processes.csv',
        'docs/external-cases/pybind11-5626/repeat-differences.csv',
        'docs/external-cases/pybind11-5626/reproduce.py',
        'docs/external-cases/pybind11-5626/summarize.py',
        'docs/external-cases/pybind11-5626/verify.py',
        'docs/noise-folding-evaluation/EVIDENCE.json',
        'docs/noise-folding-evaluation/EXPECTED_CLUES.md',
        'docs/noise-folding-evaluation/README.md',
        'docs/noise-folding-evaluation/REPRODUCE.md',
        'docs/noise-folding-evaluation/RESULTS.json',
        'docs/noise-folding-evaluation/RULES.md',
        'docs/noise-folding-evaluation/audit.py',
        'docs/noise-folding-evaluation/collect.py',
        'docs/noise-folding-evaluation/test_view.py',
        'docs/noise-folding-evaluation/view.py',
        'docs/packaging/BUILD_PATHS.md',
        'docs/packaging/PACKAGE_POLICY.md',
        'docs/packaging/check_package.py',
        'docs/performance/a3-completion-check/CALIBRATION.json',
        'docs/performance/a3-completion-check/DESIGN.md',
        'docs/performance/a3-completion-check/MEASUREMENTS.md',
        'docs/performance/a3-completion-check/OPTIMIZATION.md',
        'docs/performance/a3-completion-check/PLAN.md',
        'docs/performance/a3-completion-check/README.md',
        'docs/performance/a3-completion-check/RECORD_CHECKS.json',
        'docs/performance/a3-completion-check/RESULTS.json',
        'docs/performance/a3-completion-check/calibrate.py',
        'docs/performance/a3-completion-check/compare.py',
        'docs/performance/a3-completion-check/measure.py',
        'docs/performance/a3-completion-check/optimization/AUDIT_CLOSURE.json',
        'docs/performance/a3-completion-check/optimization/DEV_AFTER.json',
        'docs/performance/a3-completion-check/optimization/FIELD_REPLAY.json',
        'docs/performance/a3-completion-check/optimization/PILOT_DEV_AFTER.json',
        'docs/performance/a3-completion-check/optimization/PROBE_FAILURES.json',
        'docs/performance/a3-completion-check/optimization/RECORD_CHECKS.json',
        'docs/performance/a3-completion-check/optimization/RELEASE_AFTER.json',
        'docs/performance/a3-completion-check/optimization/RELEASE_BEFORE.json',
        'docs/performance/a3-completion-check/optimization/SEMANTICS.json',
        'docs/performance/a3-completion-check/optimization/TEST_RESULTS.json',
        'docs/performance/a3-completion-check/probe_gdb.py',
        'docs/performance/a3-completion-check/synthetic_backend.py',
        'docs/performance/a3-completion-check/verify.py',
        'docs/performance/a3-completion-check/verify_optimization.py',
        'docs/performance/a3-completion-check/verify_records.py',
        'docs/performance/rf600-streaming/README.md',
        'examples/field_cases.py',
        'examples/holdout_cases.py',
        'examples/probe.c',
        'examples/run.py',
        'src/capture/completion.rs',
        'src/capture/owner.rs',
        'src/capture/platform_exit.rs',
        'src/capture/policy.rs',
        'src/capture/reduction.rs',
        'src/capture/supervision.rs',
        'src/store/deletion.rs',
        'src/store/identity_tests.rs',
        'tests/acceptance.rs',
        'tests/acceptance_support.py',
        'tests/audit.py',
        'tests/capture_owner_support.py',
        'tests/capture_runner.py',
        'tests/check_cwd_tree_warning_contract.py',
        'tests/check_public_archive_freeze.py',
        'tests/check_regression_freeze.py',
        'tests/check_release_closure.py',
        'tests/completion_equivalence.py',
        'tests/contract_baseline.rs',
        'tests/cwd_tree_warning_contract.py',
        'tests/cwd_uncertainty.py',
        'tests/field_report_audit.py',
        'tests/field_warning_contract.py',
        'tests/field_warning_mutations.py',
        'tests/fixtures/audit_probe.c',
        'tests/fixtures/contract_baseline/README.md',
        'tests/fixtures/contract_baseline/baseline_manifest.json',
        'tests/fixtures/contract_baseline/golden/diff_0.json',
        'tests/fixtures/contract_baseline/golden/diff_0.txt',
        'tests/fixtures/contract_baseline/golden/diff_1.json',
        'tests/fixtures/contract_baseline/golden/diff_1.txt',
        'tests/fixtures/contract_baseline/golden/diff_2.json',
        'tests/fixtures/contract_baseline/golden/diff_2.txt',
        'tests/fixtures/contract_baseline/golden/diff_3.json',
        'tests/fixtures/contract_baseline/golden/diff_3.txt',
        'tests/fixtures/contract_baseline/golden/list.json',
        'tests/fixtures/contract_baseline/golden/list.txt',
        'tests/fixtures/contract_baseline/golden/show.json',
        'tests/fixtures/contract_baseline/golden/show.txt',
        'tests/fixtures/contract_baseline/storage/runs/abc001-101/events.jsonl',
        'tests/fixtures/contract_baseline/storage/runs/abc001-101/finalized.json',
        'tests/fixtures/contract_baseline/storage/runs/abc001-101/metadata.json',
        'tests/fixtures/contract_baseline/storage/runs/abc001-101/raw/trace.log',
        'tests/fixtures/contract_baseline/storage/runs/abc002-102/events.jsonl',
        'tests/fixtures/contract_baseline/storage/runs/abc002-102/finalized.json',
        'tests/fixtures/contract_baseline/storage/runs/abc002-102/metadata.json',
        'tests/fixtures/contract_baseline/storage/runs/abc002-102/raw/trace.log',
        'tests/fixtures/contract_baseline/storage/runs/abc101-201/events.jsonl',
        'tests/fixtures/contract_baseline/storage/runs/abc101-201/metadata.json',
        'tests/fixtures/contract_baseline/storage/runs/abc101-201/raw/trace.log',
        'tests/fixtures/contract_baseline/storage/runs/abc102-202/events.jsonl',
        'tests/fixtures/contract_baseline/storage/runs/abc102-202/metadata.json',
        'tests/fixtures/contract_baseline/storage/runs/abc102-202/raw/trace.log',
        'tests/fixtures/cwd_probe.c',
        'tests/fixtures/cwd_tree_warning_contract_v1.json',
        'tests/fixtures/field_audit_contract_v2.json',
        'tests/fixtures/field_report_audit_v1.py',
        'tests/fixtures/fs_context_probe.c',
        'tests/fixtures/fs_synthetic_backend.py',
        'tests/fixtures/prctl_fail.c',
        'tests/fixtures/pthread_eagain.c',
        'tests/fixtures/runtime_boundary_probe.c',
        'tests/fixtures/version_backend.py',
        'tests/fresh_field_audit.py',
        'tests/fresh_field_contract.py',
        'tests/fs_context.py',
        'tests/full_gate.py',
        'tests/mount_namespace.py',
        'tests/namespace_unshare.py',
        'tests/probes/capture_caught_control.c',
        'tests/probes/capture_chld_target.c',
        'tests/probes/capture_host_hooks.c',
        'tests/probes/capture_request_fault.c',
        'tests/probes/capture_syscall.c',
        'tests/probes/deletion_protocol.c',
        'tests/probes/record_restore_failure.c',
        'tests/probes/staging_cleanup_late_swap.c',
        'tests/reliability.py',
        'tests/runtime_boundary.py',
        'tests/shared_model_cli.rs',
        'tests/storage_cli.py',
        'tests/test_cwd_tree_warning_contract.py',
        'tests/test_field_warning_contract.py',
        'tests/test_fresh_field_audit.py',
        'tests/test_public_archive_freeze.py',
        'tests/test_release_closure.py',
    }
)

# The owner contract requires a reviewed README update. It is an explicit
# candidate asset, never an unstaged worktree substitution or package exception.
REQUIRED_EXISTING_INPUTS = frozenset({"tests/acceptance.rs", "README.md"})
IMMUTABLE_EXISTING_PATHS = (
    "docs/external-alpha/attribution/probe.c",
    "docs/performance/a3-completion-check/README.md",
)
# Reviewed fixture additions are pinned independently of worktree bytes.
REVIEWED_FIXTURE_ADDITIONS = {'tests/fixtures/cwd_tree_warning_contract_v1.json': '3f6620b35e004ab3bd1b5eca1c5a6fc3de418816130e2712aa89a1aaa52fd1ad', 'tests/fixtures/field_audit_contract_v2.json': '2af8f4cd2181ed0a56bcaef76bf8d69e149f31a1631b59ed88871d5d32127f71'}
# Only these reviewed historical files may first enter the public baseline.
REVIEWED_EVIDENCE_ADDITIONS = {'docs/external-alpha/attribution/probe.c': '91e4ccbd2b9bee7410b14112f1f966fc71bf9365776f9c2c126f8842cb3bfe2d', 'docs/performance/a3-completion-check/README.md': 'f22cf9729282cf52dc2e1404540a4a0567c906e9c3e0801c99e916ab2c7c987c'}
FORBIDDEN_CHANGE_PATHS = ("AGENTS.md",)
FORBIDDEN_CHANGE_PREFIXES = (".idea/",)

CONTRACT_TESTS = (
    "normal_list_and_show_text_and_json_match_exact_goldens",
    "diff_four_way_status_text_json_and_process_exit_are_identical",
    "current_and_legacy_storage_schema_fixtures_are_valid_and_minimal",
    "review_commit_manifest_and_fixture_privacy_are_frozen",
)

EXPECTED_ACCEPTANCE_SCRIPTS = frozenset(
    {
        "examples/run.py",
        "tests/completion_equivalence.py",
        "tests/cwd_uncertainty.py",
        "tests/fs_context.py",
        "tests/mount_namespace.py",
        "tests/reliability.py",
        "tests/runtime_boundary.py",
        "tests/storage_cli.py",
    }
)

DOC_REFERENCES = (
    (
        "docs/REFACTOR_STATUS.md",
        "REFACTOR_DECISIONS.md",
        "docs/REFACTOR_DECISIONS.md",
    ),
    (
        "docs/REFACTOR_STATUS.md",
        "performance/rf600-streaming/README.md",
        "docs/performance/rf600-streaming/README.md",
    ),
    (
        "docs/REFACTOR_STATUS.md",
        "tests/full_gate.py",
        "tests/full_gate.py",
    ),
)

REPOSITORY_PATH = re.compile(
    r"^(?:tests|docs|examples)/[A-Za-z0-9_.+/-]+\.(?:c|json|md|py|rs|txt)$"
)
ACCEPTANCE_SCRIPT = re.compile(r'\.arg\(\s*"([^"\n]+\.py)"\s*\)')
INCLUDE_BYTES = re.compile(r'include_bytes!\(\s*"([^"\n]+)"\s*\)')


class ClosureError(RuntimeError):
    """Candidate metadata could not be read safely."""


@dataclass(frozen=True)
class Entry:
    """One stage-zero regular Git entry."""

    mode: str
    oid: str


class Candidate:
    """Read-only view of blobs named by an explicit index or Git tree."""

    def __init__(self, repo: Path, entries: dict[str, Entry], label: str):
        self.repo = repo
        self.entries = entries
        self.label = label
        self._blob_cache: dict[str, bytes] = {}

    @classmethod
    def from_index(cls, repo: Path, index: Path) -> "Candidate":
        index = index.resolve()
        if not index.is_file():
            raise ClosureError(f"candidate index is not a regular file: {index}")
        environment = os.environ.copy()
        environment["GIT_INDEX_FILE"] = str(index)
        output = git(repo, "ls-files", "--stage", "-z", environment=environment)
        entries: dict[str, Entry] = {}
        for record in output.split(b"\0"):
            if not record:
                continue
            try:
                header, raw_path = record.split(b"\t", 1)
                mode, oid, stage_number = header.decode("ascii").split()
                path = raw_path.decode("utf-8")
            except (UnicodeDecodeError, ValueError) as error:
                raise ClosureError(f"cannot decode candidate index entry: {record!r}") from error
            if stage_number != "0":
                raise ClosureError(f"candidate index contains an unmerged entry: {path}")
            entries[path] = Entry(mode, oid)
        return cls(repo, entries, f"index {index}")

    @classmethod
    def from_tree(cls, repo: Path, treeish: str) -> "Candidate":
        output = git(repo, "ls-tree", "-r", "-z", "--full-tree", treeish)
        entries: dict[str, Entry] = {}
        for record in output.split(b"\0"):
            if not record:
                continue
            try:
                header, raw_path = record.split(b"\t", 1)
                mode, object_type, oid = header.decode("ascii").split()
                path = raw_path.decode("utf-8")
            except (UnicodeDecodeError, ValueError) as error:
                raise ClosureError(f"cannot decode candidate tree entry: {record!r}") from error
            if object_type != "blob":
                raise ClosureError(f"candidate path is not a blob: {path}")
            entries[path] = Entry(mode, oid)
        return cls(repo, entries, f"tree {treeish}")

    def read(self, path: str) -> bytes:
        entry = self.entries.get(path)
        if entry is None:
            raise KeyError(path)
        if path not in self._blob_cache:
            self._blob_cache[path] = git(self.repo, "cat-file", "blob", entry.oid)
        return self._blob_cache[path]


def git(
    repo: Path,
    *arguments: str,
    environment: dict[str, str] | None = None,
) -> bytes:
    """Run one read-only Git query and return its exact stdout bytes."""

    result = subprocess.run(
        ["git", "-C", str(repo), *arguments],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        diagnostic = result.stderr.decode("utf-8", errors="replace").strip()
        raise ClosureError(
            f"git {' '.join(arguments)} failed with {result.returncode}: {diagnostic}"
        )
    return result.stdout


def regular_worktree_bytes(repo: Path, path: str, errors: list[str]) -> bytes | None:
    """Read a required ordinary worktree file without following a symlink."""

    absolute = repo / path
    try:
        metadata = absolute.lstat()
    except FileNotFoundError:
        errors.append(f"required worktree asset is missing: {path}")
        return None
    if not stat.S_ISREG(metadata.st_mode):
        errors.append(f"required worktree asset is not a regular file: {path}")
        return None
    try:
        return absolute.read_bytes()
    except OSError as error:
        errors.append(f"cannot read required worktree asset {path}: {error}")
        return None


def require_matching_blob(
    repo: Path,
    candidate: Candidate,
    path: str,
    errors: list[str],
) -> bytes | None:
    """Require one candidate blob and exact equality with its worktree asset."""

    entry = candidate.entries.get(path)
    if entry is None:
        errors.append(f"missing from candidate: {path}")
        return None
    if entry.mode not in {"100644", "100755"}:
        errors.append(f"candidate asset is not a regular file: {path} ({entry.mode})")
        return None
    worktree = regular_worktree_bytes(repo, path, errors)
    if worktree is None:
        return None
    candidate_bytes = candidate.read(path)
    if candidate_bytes != worktree:
        errors.append(f"candidate blob differs from worktree bytes: {path}")
        return None
    return candidate_bytes


def python_paths(source: bytes, source_path: str, errors: list[str]) -> set[str]:
    """Extract literal repository file dependencies from one Python source."""

    try:
        text = source.decode("utf-8")
        tree = ast.parse(text, filename=source_path)
    except (UnicodeDecodeError, SyntaxError) as error:
        errors.append(f"cannot parse Python dependency source {source_path}: {error}")
        return set()
    paths: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            spelling = node.value.removeprefix("./")
            if REPOSITORY_PATH.fullmatch(spelling):
                paths.add(spelling)
    return paths


def normalized_relative(base: PurePosixPath, spelling: str) -> str | None:
    """Resolve one source-relative literal without permitting upward traversal."""

    combined = base / spelling
    parts: list[str] = []
    for part in combined.parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not parts:
                return None
            parts.pop()
        else:
            parts.append(part)
    return "/".join(parts)


def validate_contract(
    repo: Path,
    candidate: Candidate,
    source: bytes | None,
    errors: list[str],
) -> None:
    """Freeze all four RF-000 tests and every runtime/compile-time fixture."""

    if source is None:
        return
    try:
        text = source.decode("utf-8")
    except UnicodeDecodeError as error:
        errors.append(f"contract baseline source is not UTF-8: {error}")
        return
    registered_tests = set(
        re.findall(r"#\s*\[\s*test\s*\]\s*fn\s+([A-Za-z0-9_]+)\s*\(", text)
    )
    for test in sorted(set(CONTRACT_TESTS) - registered_tests):
        errors.append(f"contract baseline #[test] is missing: {test}")
    for test in sorted(registered_tests - set(CONTRACT_TESTS)):
        errors.append(f"unexpected contract baseline #[test]: {test}")
    included = {
        normalized_relative(PurePosixPath("tests"), spelling)
        for spelling in INCLUDE_BYTES.findall(text)
    }
    if None in included:
        errors.append("contract baseline include_bytes! escapes the repository")
    expected_includes = {
        path for path in CONTRACT_ASSETS if "/golden/" in path
    }
    missing_includes = expected_includes - included
    for path in sorted(missing_includes):
        errors.append(f"contract golden is not included by the test: {path}")
    for path in CONTRACT_ASSETS:
        require_matching_blob(repo, candidate, path, errors)


def validate_doc_references(
    repo: Path,
    candidate: Candidate,
    errors: list[str],
) -> None:
    """Require the new status document's explicitly published local references."""

    cache: dict[str, bytes | None] = {}
    for source_path, spelling, target_path in DOC_REFERENCES:
        if source_path not in cache:
            cache[source_path] = require_matching_blob(
                repo, candidate, source_path, errors
            )
        source = cache[source_path]
        if source is not None and spelling.encode() not in source:
            errors.append(
                f"required documentation reference {spelling!r} is absent from {source_path}"
            )
        require_matching_blob(repo, candidate, target_path, errors)


def validate_unchanged_baseline(
    candidate: Candidate,
    baseline: Candidate,
    errors: list[str],
) -> None:
    """Reject accidental new/private files without rejecting unchanged baseline paths."""

    reviewed_additions = {**REVIEWED_FIXTURE_ADDITIONS, **REVIEWED_EVIDENCE_ADDITIONS}
    protected_paths = set(IMMUTABLE_EXISTING_PATHS) | set(reviewed_additions)
    for path in sorted(protected_paths):
        # A reviewed first addition is an exception only to baseline absence.
        # Once present, even reviewed bytes cannot authorize changing/deleting it.
        if path not in baseline.entries and path in reviewed_additions:
            if path in candidate.entries and hashlib.sha256(candidate.read(path)).hexdigest() != reviewed_additions[path]:
                kind = "evidence" if path in REVIEWED_EVIDENCE_ADDITIONS else "fixture"
                errors.append(f"reviewed {kind} addition hash mismatch: {path}")
            continue
        if candidate.entries.get(path) != baseline.entries.get(path):
            errors.append(f"candidate changes excluded existing path: {path}")
    changed_paths = set(candidate.entries) | set(baseline.entries)
    for path in sorted(changed_paths):
        if candidate.entries.get(path) == baseline.entries.get(path):
            continue
        if path in FORBIDDEN_CHANGE_PATHS or path.startswith(FORBIDDEN_CHANGE_PREFIXES):
            errors.append(f"candidate changes forbidden private path: {path}")
    for path in sorted(set(candidate.entries) - set(baseline.entries)):
        if path not in NEW_RELEASE_ASSETS:
            errors.append(f"candidate adds path outside the release asset allowlist: {path}")


def validate_release_closure(repo: Path, candidate: Candidate) -> list[str]:
    """Return every deterministic closure violation for one candidate snapshot."""

    errors: list[str] = []
    baseline = Candidate.from_tree(repo, "HEAD")
    validate_unchanged_baseline(candidate, baseline, errors)

    for path in sorted(NEW_RELEASE_ASSETS):
        if path in IMMUTABLE_EXISTING_PATHS and path in baseline.entries:
            # Historical baseline evidence is sealed by its candidate blob;
            # unstaged local evidence is outside this candidate, not substituted.
            continue
        require_matching_blob(repo, candidate, path, errors)
    for path in sorted(REQUIRED_EXISTING_INPUTS):
        require_matching_blob(repo, candidate, path, errors)
    acceptance = require_matching_blob(
        repo, candidate, "tests/acceptance.rs", errors
    )
    if acceptance is not None:
        try:
            acceptance_text = acceptance.decode("utf-8")
        except UnicodeDecodeError as error:
            errors.append(f"tests/acceptance.rs is not UTF-8: {error}")
            acceptance_text = ""
        scripts = set(ACCEPTANCE_SCRIPT.findall(acceptance_text))
        for expected in sorted(EXPECTED_ACCEPTANCE_SCRIPTS - scripts):
            errors.append(f"acceptance registration is missing: {expected}")
    else:
        scripts = set()

    pending_python = set(scripts)
    inspected_python: set[str] = set()
    while pending_python:
        path = min(pending_python)
        pending_python.remove(path)
        if path in inspected_python:
            continue
        inspected_python.add(path)
        source = require_matching_blob(repo, candidate, path, errors)
        if source is None:
            continue
        dependencies = python_paths(source, path, errors)
        for dependency in sorted(dependencies):
            require_matching_blob(repo, candidate, dependency, errors)
            if (
                dependency.startswith("tests/fixtures/")
                and dependency.endswith(".py")
                and dependency not in inspected_python
            ):
                pending_python.add(dependency)

    # Full-gate repository paths are direct executable/data dependencies.  Do
    # not recursively interpret the policy checker's constants as dependencies:
    # its allowlists intentionally name excluded baseline paths and negative-test
    # examples that must not be pulled into the candidate.
    full_gate = require_matching_blob(repo, candidate, "tests/full_gate.py", errors)
    if full_gate is not None:
        for dependency in sorted(python_paths(full_gate, "tests/full_gate.py", errors)):
            require_matching_blob(repo, candidate, dependency, errors)

    contract = require_matching_blob(
        repo, candidate, "tests/contract_baseline.rs", errors
    )
    validate_contract(repo, candidate, contract, errors)
    validate_doc_references(repo, candidate, errors)
    return sorted(set(errors))


def parse_arguments(arguments: Iterable[str] | None = None) -> argparse.Namespace:
    """Parse an explicit read-only candidate selection."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        type=Path,
        default=ROOT,
        help="repository root (defaults to this script's repository)",
    )
    selection = parser.add_mutually_exclusive_group(required=True)
    selection.add_argument(
        "--index",
        type=Path,
        help="explicit alternate GIT_INDEX_FILE to inspect without modifying it",
    )
    selection.add_argument(
        "--tree",
        help="existing commit/tree-ish to inspect without creating a commit",
    )
    return parser.parse_args(arguments)


def main(arguments: Iterable[str] | None = None) -> int:
    """Validate one candidate and emit a concise deterministic result."""

    args = parse_arguments(arguments)
    repo = args.repo.resolve()
    try:
        candidate = (
            Candidate.from_index(repo, args.index)
            if args.index is not None
            else Candidate.from_tree(repo, args.tree)
        )
        errors = validate_release_closure(repo, candidate)
    except ClosureError as error:
        print(f"FAIL release closure: {error}", file=sys.stderr)
        return 2
    if errors:
        print(f"FAIL release closure for {candidate.label}:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(
        f"PASS release closure for {candidate.label}: "
        f"{len(NEW_RELEASE_ASSETS)} new assets and all discovered dependencies are sealed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
