#!/usr/bin/env python3
"""Read-only public-export closure checks; diagnostics never disclose match values."""
import argparse
import ast
import base64
from collections import Counter, deque
import hashlib
import html
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib
import tokenize
import urllib.parse
import zlib

BASELINE = "78e6a274455e3f44514d0aed00b42bebee403f21"
VERSION = "0.2.0-alpha.1"
SOURCES = {
    "src/main.rs", "src/cli.rs", "src/capture/mod.rs", "src/parse/mod.rs",
    "src/parse/fs_tests.rs", "src/model/mod.rs", "src/store/mod.rs",
    "src/diff/mod.rs", "src/report/mod.rs",
}
LEGACY_PAYLOAD = SOURCES | {
    "Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "assets/rundelta-signal.svg",
    "docs/BUILDING.md", "docs/PROJECT.md",
    "tests/fixtures/trace.txt", "tests/public_cli.rs", "tests/public_cli.py",
}
LEGACY_WORKTREE = LEGACY_PAYLOAD | {
    ".gitignore", "tools/check_export.py", "tests/test_export_safety.py",
}
HISTORIC_PAYLOAD = LEGACY_PAYLOAD | {
    "docs/ARCHITECTURE.md", "docs/CHANGELOG.md",
    "tools/check_export.py", "tests/test_export_safety.py",
    "tests/fixtures/finalization_barrier.c",
}
HISTORIC_REPO = HISTORIC_PAYLOAD | {".gitignore", ".github/workflows/ci.yml"}
# Explicit reviewed inventories; repository tools/evidence never enter the crate.
REPO_FILES = {
    '.gitattributes',
    '.github/workflows/ci.yml',
    '.gitignore',
    'Cargo.lock',
    'Cargo.toml',
    'LICENSE',
    'README.md',
    'assets/rundelta-signal.svg',
    'docs/ACCEPTANCE-before-reliability.md',
    'docs/ACCEPTANCE.md',
    'docs/ALPHA_READINESS.md',
    'docs/ARCHITECTURE.md',
    'docs/ARCHITECTURE_REVIEW.md',
    'docs/BUILDING.md',
    'docs/CHANGELOG.md',
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
    'docs/PROJECT.md',
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
    'src/capture/mod.rs',
    'src/capture/owner.rs',
    'src/capture/platform_exit.rs',
    'src/capture/policy.rs',
    'src/capture/reduction.rs',
    'src/capture/supervision.rs',
    'src/cli.rs',
    'src/diff/mod.rs',
    'src/main.rs',
    'src/model/mod.rs',
    'src/parse/fs_tests.rs',
    'src/parse/mod.rs',
    'src/report/mod.rs',
    'src/store/deletion.rs',
    'src/store/identity_tests.rs',
    'src/store/mod.rs',
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
    "tests/fixtures/finalization_barrier.c",
    'tests/fixtures/fs_context_probe.c',
    'tests/fixtures/fs_synthetic_backend.py',
    'tests/fixtures/prctl_fail.c',
    'tests/fixtures/pthread_eagain.c',
    'tests/fixtures/runtime_boundary_probe.c',
    'tests/fixtures/trace.txt',
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
    'tests/public_cli.py',
    'tests/public_cli.rs',
    'tests/reliability.py',
    'tests/runtime_boundary.py',
    'tests/shared_model_cli.rs',
    'tests/storage_cli.py',
    'tests/test_cwd_tree_warning_contract.py',
    'tests/test_export_safety.py',
    'tests/test_field_warning_contract.py',
    'tests/test_fresh_field_audit.py',
    'tests/test_public_archive_freeze.py',
    'tests/test_release_closure.py',
    'tools/check_export.py',
}
CRATE_FILES = {
    'Cargo.lock',
    'Cargo.toml',
    'LICENSE',
    'docs/CRATE_README.md',
    'src/capture/completion.rs',
    'src/capture/mod.rs',
    'src/capture/owner.rs',
    'src/capture/platform_exit.rs',
    'src/capture/policy.rs',
    'src/capture/reduction.rs',
    'src/capture/supervision.rs',
    'src/cli.rs',
    'src/diff/mod.rs',
    'src/main.rs',
    'src/model/mod.rs',
    'src/parse/fs_tests.rs',
    'src/parse/mod.rs',
    'src/report/mod.rs',
    'src/store/deletion.rs',
    'src/store/mod.rs',
}
CRATE_ARCHIVE_FILES = CRATE_FILES | {"Cargo.toml.orig", ".cargo_vcs_info.json"}
EXECUTABLE_FILES = {"docs/performance/a3-completion-check/synthetic_backend.py",
                    "tests/fixtures/fs_synthetic_backend.py"}
HISTORIC_SCHEMAS = {
    BASELINE: ("8776b8532ce765e9cdd707aa10c46e2dbb0098bc", LEGACY_WORKTREE, LEGACY_PAYLOAD, True),
    "0cd264d72f3cd37451d46c1397a2a753332019c6": ("43821c85480a16ac577ca158ead8eb76007a1759", HISTORIC_REPO, HISTORIC_PAYLOAD, False),
    "051b1f5b3c800c62fbb3dc484291b026edacc381": ("7540110566af577c6bb697e57caea109d0de31c6", HISTORIC_REPO, HISTORIC_PAYLOAD, False),
}
LICENSE_SHA = "f8e9acc75d2b3ad143bedc06b425525f311dd51b9b0cb1885dff81e1fc200a7b"
PRIVATE_PATH = re.compile(
    r"/(?:home|Users|root|tmp|var/tmp|var/folders|private/tmp|private/var/folders|run/user)/"
    r"|[A-Za-z]:[\\/]Users[\\/]", re.I)
MATERIAL = re.compile(
    r"(?i)(?:docs[/\\](?:external[-]alpha|external[-]cases|performance|noise[-]folding)"
    r"|(?:^|[/\\])\.idea(?:[/\\]|$)|(?:^|[/\\])AGENTS\.md(?:$|[\s\"'])"
    r"|(?:ACCEPTANCE|ARCHITECTURE_REVIEW|FIELD_CASES|HOLDOUT_RESULTS|RANKING_REPAIR"
    r"|REFACTOR_STATUS|REFACTOR_DECISIONS)[A-Za-z0-9_-]*\.(?:md|json)"
    r"|docs[/\\][A-Za-z0-9_./\\-]+\.(?:md|json|csv|py|c))")
SECRET = re.compile(
    r"gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}"
    r"|(?:AKIA|ASIA)[0-9A-Z]{16}|sk-[A-Za-z0-9_-]{20,}"
    r"|xox[baprs]-[A-Za-z0-9-]{20,}|glpat-[A-Za-z0-9_-]{20,}"
    r"|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"
    r"|https?://[^\s/@:]+:[^\s/@]+@")
TEMP_ROOT = chr(47) + "tmp" + chr(47)
# Reviewed synthetic Rust test operands only; neither path prefixes nor arbitrary
# strings in the named file are exempt. Historical exemptions stay historical.
CONSTANTS = {"src/report/mod.rs": {
    TEMP_ROOT + "rustcABC123/key", TEMP_ROOT + "ccAb1234.s",
    TEMP_ROOT + "important.conf", TEMP_ROOT + "ccX.s",
}}
LEGACY_CONSTANTS = {
    "src/diff/mod.rs": {TEMP_ROOT, TEMP_ROOT + "ccAb1234.s",
                        TEMP_ROOT + "important.conf", TEMP_ROOT + "ccX.s"},
    "src/report/mod.rs": {TEMP_ROOT + "rustcABC123/key"},
}
HEX = re.compile(r"(?<![0-9A-Za-z])(?:[0-9a-fA-F]{2}){4,}(?![0-9A-Za-z])")
B64 = re.compile(r"(?<![A-Za-z0-9_+/=-])[A-Za-z0-9_+/-]{2,}={0,2}(?![A-Za-z0-9_+/=-])")
WRAPPED_B64 = re.compile(r"(?m)^[A-Za-z0-9_+/-]{4,}={0,2}(?:\r?\n[A-Za-z0-9_+/-]{4,}={0,2})+$")

PUBLISHED_REFERENCES = REPO_FILES
SOURCE_REFERENCES = {'README.md': {'REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json'}, 'docs/ACCEPTANCE.md': {'REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json', 'ACCEPTANCE-before-reliability.md': 'docs/ACCEPTANCE-before-reliability.md', 'FIELD_CASES.md': 'docs/FIELD_CASES.md', 'RANKING_REPAIR.md': 'docs/RANKING_REPAIR.md', 'HOLDOUT_PLAN.md': 'docs/HOLDOUT_PLAN.md', 'HOLDOUT_EVALUATION.md': 'docs/HOLDOUT_EVALUATION.md', 'INCOMPLETE_CAPTURE.md': 'docs/INCOMPLETE_CAPTURE.md', 'OPTIMIZATION.md': 'docs/performance/a3-completion-check/OPTIMIZATION.md', 'performance/a3-completion-check/OPTIMIZATION.md': 'docs/performance/a3-completion-check/OPTIMIZATION.md', 'audit.py': 'docs/noise-folding-evaluation/audit.py', 'performance/a3-completion-check/optimization/TEST_RESULTS.json': 'docs/performance/a3-completion-check/optimization/TEST_RESULTS.json', 'TEST_RESULTS.json': 'docs/performance/a3-completion-check/optimization/TEST_RESULTS.json', 'AUDIT_CLOSURE.json': 'docs/performance/a3-completion-check/optimization/AUDIT_CLOSURE.json', 'performance/a3-completion-check/optimization/AUDIT_CLOSURE.json': 'docs/performance/a3-completion-check/optimization/AUDIT_CLOSURE.json', 'FS_CONTEXT_MODEL.md': 'docs/FS_CONTEXT_MODEL.md', 'measure.py': 'docs/performance/a3-completion-check/measure.py', 'compare.py': 'docs/performance/a3-completion-check/compare.py', 'FIELD_AUDIT_LAYERS.md': 'docs/FIELD_AUDIT_LAYERS.md'}, 'docs/ALPHA_READINESS.md': {'REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json', 'REFACTOR_STATUS.md': 'docs/REFACTOR_STATUS.md', 'packaging/PACKAGE_POLICY.md': 'docs/packaging/PACKAGE_POLICY.md', 'packaging/BUILD_PATHS.md': 'docs/packaging/BUILD_PATHS.md', 'docs/external-alpha/batch-2/': 'docs/external-alpha/batch-2/', 'SCREENING.md': 'docs/external-alpha/batch-2/SCREENING.md', 'analyze.py': 'docs/external-alpha/batch-2/analyze.py', 'test_redaction.py': 'docs/external-alpha/batch-2/test_redaction.py', 'FS_CONTEXT_MODEL.md': 'docs/FS_CONTEXT_MODEL.md', 'ARCHITECTURE_REVIEW.md': 'docs/ARCHITECTURE_REVIEW.md'}, 'docs/ARCHITECTURE.md': {'../README.md': 'README.md', 'PROJECT.md': 'docs/PROJECT.md'}, 'docs/ARCHITECTURE_REVIEW.md': {'REFACTOR_DECISIONS.md': 'docs/REFACTOR_DECISIONS.md', 'ACCEPTANCE.md': 'docs/ACCEPTANCE.md', 'FS_CONTEXT_MODEL.md': 'docs/FS_CONTEXT_MODEL.md', 'FIELD_AUDIT_LAYERS.md': 'docs/FIELD_AUDIT_LAYERS.md', 'docs/noise-folding-evaluation/': 'docs/noise-folding-evaluation/', 'EXPECTED_CLUES.md': 'docs/noise-folding-evaluation/EXPECTED_CLUES.md', 'RULES.md': 'docs/noise-folding-evaluation/RULES.md', 'audit.py': 'docs/noise-folding-evaluation/audit.py', 'test_view.py': 'docs/noise-folding-evaluation/test_view.py', 'view.py': 'docs/noise-folding-evaluation/view.py', '../src/store/mod.rs': 'src/store/mod.rs', '../src/capture/mod.rs': 'src/capture/mod.rs', '../src/parse/mod.rs': 'src/parse/mod.rs', '../src/diff/mod.rs': 'src/diff/mod.rs', 'performance/a3-completion-check/OPTIMIZATION.md': 'docs/performance/a3-completion-check/OPTIMIZATION.md', 'performance/a3-completion-check/optimization/AUDIT_CLOSURE.json': 'docs/performance/a3-completion-check/optimization/AUDIT_CLOSURE.json', '../src/main.rs': 'src/main.rs', '../src/report/mod.rs': 'src/report/mod.rs', '../src/model/mod.rs': 'src/model/mod.rs', '../src/cli.rs': 'src/cli.rs', '../examples/run.py': 'examples/run.py'}, 'docs/BUILDING.md': {'../README.md': 'README.md', 'PROJECT.md': 'docs/PROJECT.md'}, 'docs/CRATE_README.md': {'../LICENSE': 'LICENSE'}, 'docs/FIELD_CASES.md': {'RANKING_REPAIR.md': 'docs/RANKING_REPAIR.md', '../examples/field_cases.py': 'examples/field_cases.py', 'FIELD_CASES_RESULTS.json': 'docs/FIELD_CASES_RESULTS.json', 'ACCEPTANCE.md': 'docs/ACCEPTANCE.md'}, 'docs/FS_CONTEXT_MODEL.md': {'REFACTOR_DECISIONS.md': 'docs/REFACTOR_DECISIONS.md', 'FIELD_AUDIT_LAYERS.md': 'docs/FIELD_AUDIT_LAYERS.md'}, 'docs/HOLDOUT_EVALUATION.md': {'REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json', 'HOLDOUT_PLAN.md': 'docs/HOLDOUT_PLAN.md', 'HOLDOUT_RESULTS.json': 'docs/HOLDOUT_RESULTS.json', 'REGRESSION_SET.md': 'docs/REGRESSION_SET.md', '../examples/holdout_cases.py': 'examples/holdout_cases.py', 'INCOMPLETE_CAPTURE.md': 'docs/INCOMPLETE_CAPTURE.md'}, 'docs/HOLDOUT_PLAN.md': {'REGRESSION_SET.md': 'docs/REGRESSION_SET.md'}, 'docs/HOLDOUT_RESULTS.json': {'HOLDOUT_PLAN.md': 'docs/HOLDOUT_PLAN.md'}, 'docs/INCOMPLETE_CAPTURE.md': {'FS_CONTEXT_MODEL.md': 'docs/FS_CONTEXT_MODEL.md', 'FIELD_AUDIT_LAYERS.md': 'docs/FIELD_AUDIT_LAYERS.md', 'REGRESSION_SET.md': 'docs/REGRESSION_SET.md'}, 'docs/INDEX.md': {'REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json', '../README.md': 'README.md', 'FS_CONTEXT_MODEL.md': 'docs/FS_CONTEXT_MODEL.md', 'FIELD_AUDIT_LAYERS.md': 'docs/FIELD_AUDIT_LAYERS.md', 'REFACTOR_DECISIONS.md': 'docs/REFACTOR_DECISIONS.md', 'REFACTOR_STATUS.md': 'docs/REFACTOR_STATUS.md', 'REGRESSION_SET.md': 'docs/REGRESSION_SET.md', 'REGRESSION_SET.json': 'docs/REGRESSION_SET.json', 'INCOMPLETE_CAPTURE.md': 'docs/INCOMPLETE_CAPTURE.md', '../tests/full_gate.py': 'tests/full_gate.py', '../examples/field_cases.py': 'examples/field_cases.py', 'ACCEPTANCE-before-reliability.md': 'docs/ACCEPTANCE-before-reliability.md', 'ACCEPTANCE.md': 'docs/ACCEPTANCE.md', 'ARCHITECTURE_REVIEW.md': 'docs/ARCHITECTURE_REVIEW.md', 'FIELD_CASES.md': 'docs/FIELD_CASES.md', 'FIELD_CASES_RESULTS.json': 'docs/FIELD_CASES_RESULTS.json', 'RANKING_REPAIR.md': 'docs/RANKING_REPAIR.md', 'RANKING_REPAIR_RESULTS.json': 'docs/RANKING_REPAIR_RESULTS.json', 'HOLDOUT_PLAN.md': 'docs/HOLDOUT_PLAN.md', 'HOLDOUT_EVALUATION.md': 'docs/HOLDOUT_EVALUATION.md', 'HOLDOUT_RESULTS.json': 'docs/HOLDOUT_RESULTS.json', 'noise-folding-evaluation/README.md': 'docs/noise-folding-evaluation/README.md', 'noise-folding-evaluation/RULES.md': 'docs/noise-folding-evaluation/RULES.md', 'noise-folding-evaluation/EXPECTED_CLUES.md': 'docs/noise-folding-evaluation/EXPECTED_CLUES.md', 'noise-folding-evaluation/REPRODUCE.md': 'docs/noise-folding-evaluation/REPRODUCE.md', 'external-alpha/README.md': 'docs/external-alpha/README.md', 'external-alpha/CHECKS.md': 'docs/external-alpha/CHECKS.md', 'external-alpha/REPRODUCE.md': 'docs/external-alpha/REPRODUCE.md', 'external-alpha/pybind11-3445/CASE.md': 'docs/external-alpha/pybind11-3445/CASE.md', 'external-alpha/spades-1339/CASE.md': 'docs/external-alpha/spades-1339/CASE.md', 'external-alpha/meson-16136/CASE.md': 'docs/external-alpha/meson-16136/CASE.md', 'external-alpha/attribution/README.md': 'docs/external-alpha/attribution/README.md', 'external-alpha/clone-fs-attribution/README.md': 'docs/external-alpha/clone-fs-attribution/README.md', 'external-alpha/batch-2/README.md': 'docs/external-alpha/batch-2/README.md', 'external-alpha/batch-2/SCREENING.md': 'docs/external-alpha/batch-2/SCREENING.md', 'external-alpha/batch-2/CHECKS.md': 'docs/external-alpha/batch-2/CHECKS.md', 'external-cases/pybind11-5626/README.md': 'docs/external-cases/pybind11-5626/README.md', 'external-cases/pybind11-5626/FROZEN.md': 'docs/external-cases/pybind11-5626/FROZEN.md', 'external-cases/pybind11-5626/REPORT.md': 'docs/external-cases/pybind11-5626/REPORT.md', 'external-cases/pybind11-5626/NOISE_ANALYSIS.md': 'docs/external-cases/pybind11-5626/NOISE_ANALYSIS.md', 'performance/a3-completion-check/README.md': 'docs/performance/a3-completion-check/README.md', 'performance/a3-completion-check/PLAN.md': 'docs/performance/a3-completion-check/PLAN.md', 'performance/a3-completion-check/MEASUREMENTS.md': 'docs/performance/a3-completion-check/MEASUREMENTS.md', 'performance/a3-completion-check/DESIGN.md': 'docs/performance/a3-completion-check/DESIGN.md', 'performance/a3-completion-check/OPTIMIZATION.md': 'docs/performance/a3-completion-check/OPTIMIZATION.md', 'performance/rf600-streaming/README.md': 'docs/performance/rf600-streaming/README.md', 'CRATE_README.md': 'docs/CRATE_README.md', 'ALPHA_READINESS.md': 'docs/ALPHA_READINESS.md', 'packaging/PACKAGE_POLICY.md': 'docs/packaging/PACKAGE_POLICY.md', 'packaging/BUILD_PATHS.md': 'docs/packaging/BUILD_PATHS.md', 'packaging/check_package.py': 'docs/packaging/check_package.py'}, 'docs/PROJECT.md': {'../README.md': 'README.md', 'ARCHITECTURE.md': 'docs/ARCHITECTURE.md', 'BUILDING.md': 'docs/BUILDING.md'}, 'docs/RANKING_REPAIR.md': {'FIELD_CASES.md': 'docs/FIELD_CASES.md', 'RANKING_REPAIR_RESULTS.json': 'docs/RANKING_REPAIR_RESULTS.json'}, 'docs/REFACTOR_STATUS.md': {'FS_CONTEXT_MODEL.md': 'docs/FS_CONTEXT_MODEL.md', 'REFACTOR_DECISIONS.md': 'docs/REFACTOR_DECISIONS.md', 'performance/rf600-streaming/README.md': 'docs/performance/rf600-streaming/README.md', 'CRATE_README.md': 'docs/CRATE_README.md'}, 'docs/REGRESSION_SET.md': {'REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json', 'REGRESSION_SET.json': 'docs/REGRESSION_SET.json', 'FIELD_AUDIT_LAYERS.md': 'docs/FIELD_AUDIT_LAYERS.md', 'FIELD_CASES.md': 'docs/FIELD_CASES.md'}, 'docs/external-alpha/CHECKS.md': {'REPLAY.json': 'docs/external-alpha/REPLAY.json'}, 'docs/external-alpha/README.md': {'provenance.json': 'docs/external-alpha/provenance.json', 'REPRODUCE.md': 'docs/external-alpha/REPRODUCE.md', 'verify.py': 'docs/external-alpha/verify.py', 'REPLAY.json': 'docs/external-alpha/REPLAY.json'}, 'docs/external-alpha/attribution/README.md': {'../../../src/parse/mod.rs': 'src/parse/mod.rs', '../../../src/capture/mod.rs': 'src/capture/mod.rs', '../../../src/diff/mod.rs': 'src/diff/mod.rs', 'RESULTS.json': 'docs/external-alpha/attribution/RESULTS.json', 'probe.c': 'docs/external-alpha/attribution/probe.c', '../verify.py': 'docs/external-alpha/verify.py', 'probe.py': 'docs/external-alpha/attribution/probe.py'}, 'docs/external-alpha/attribution/original-materials.json': {'REPLAY.json': 'docs/external-alpha/REPLAY.json', 'preservation.json': 'docs/external-alpha/preservation.json', 'verify.py': 'docs/external-alpha/attribution/verify.py'}, 'docs/external-alpha/attribution/probe.py': {'probe.c': 'docs/external-alpha/attribution/probe.c'}, 'docs/external-alpha/attribution/verify.py': {'original-materials.json': 'docs/external-alpha/attribution/original-materials.json', 'RESULTS.json': 'docs/external-alpha/attribution/RESULTS.json', 'probe.c': 'docs/external-alpha/attribution/probe.c', 'probe.py': 'docs/external-alpha/attribution/probe.py', 'verify.py': 'docs/external-alpha/attribution/verify.py'}, 'docs/external-alpha/batch-2/README.md': {'SCREENING.md': 'docs/external-alpha/batch-2/SCREENING.md', 'pip-12996/CASE.md': 'docs/external-alpha/batch-2/pip-12996/CASE.md', 'bracket-lib-376/CASE.md': 'docs/external-alpha/batch-2/bracket-lib-376/CASE.md', 'provenance.json': 'docs/external-alpha/batch-2/provenance.json', 'pip-12996/RESULTS.md': 'docs/external-alpha/batch-2/pip-12996/RESULTS.md', 'bracket-lib-376/RESULTS.md': 'docs/external-alpha/batch-2/bracket-lib-376/RESULTS.md', 'reproduce.py': 'docs/external-alpha/batch-2/reproduce.py', 'analyze.py': 'docs/external-alpha/batch-2/analyze.py', 'CHECKS.md': 'docs/external-alpha/batch-2/CHECKS.md', 'verify.py': 'docs/external-alpha/batch-2/verify.py'}, 'docs/external-alpha/batch-2/SCREENING.md': {'provenance.json': 'docs/external-alpha/batch-2/provenance.json'}, 'docs/external-alpha/batch-2/bracket-lib-376/RESULTS.md': {'CASE.md': 'docs/external-alpha/batch-2/bracket-lib-376/CASE.md', '../reproduce.py': 'docs/external-alpha/batch-2/reproduce.py', 'summary.json': 'docs/external-alpha/batch-2/bracket-lib-376/summary.json', 'evidence.json': 'docs/external-alpha/batch-2/bracket-lib-376/evidence.json'}, 'docs/external-alpha/batch-2/original-materials.json': {'CHECKS.md': 'docs/external-alpha/batch-2/CHECKS.md', 'REPLAY.json': 'docs/external-alpha/REPLAY.json', 'verify.py': 'docs/external-alpha/batch-2/verify.py', 'preservation.json': 'docs/external-alpha/preservation.json', 'reproduce.py': 'docs/external-alpha/batch-2/reproduce.py', 'provenance.json': 'docs/external-alpha/batch-2/provenance.json'}, 'docs/external-alpha/batch-2/pip-12996/RESULTS.md': {'CASE.md': 'docs/external-alpha/batch-2/pip-12996/CASE.md', '../reproduce.py': 'docs/external-alpha/batch-2/reproduce.py', 'summary.json': 'docs/external-alpha/batch-2/pip-12996/summary.json', 'evidence.json': 'docs/external-alpha/batch-2/pip-12996/evidence.json'}, 'docs/external-alpha/batch-2/reproduce.py': {'provenance.json': 'docs/external-alpha/batch-2/provenance.json'}, 'docs/external-alpha/batch-2/verify.py': {'CHECKS.md': 'docs/external-alpha/batch-2/CHECKS.md', 'SCREENING.md': 'docs/external-alpha/batch-2/SCREENING.md', 'provenance.json': 'docs/external-alpha/batch-2/provenance.json', 'original-materials.json': 'docs/external-alpha/batch-2/original-materials.json', 'reproduce.py': 'docs/external-alpha/batch-2/reproduce.py', 'analyze.py': 'docs/external-alpha/batch-2/analyze.py', 'verify.py': 'docs/external-alpha/batch-2/verify.py', 'test_redaction.py': 'docs/external-alpha/batch-2/test_redaction.py'}, 'docs/external-alpha/clone-fs-attribution/README.md': {'../../../src/parse/mod.rs': 'src/parse/mod.rs', '../../../src/model/mod.rs': 'src/model/mod.rs', '../../../src/capture/mod.rs': 'src/capture/mod.rs', '../../../src/report/mod.rs': 'src/report/mod.rs', '../../../src/diff/mod.rs': 'src/diff/mod.rs', 'PLAN.md': 'docs/external-alpha/clone-fs-attribution/PLAN.md', 'probe.c': 'docs/external-alpha/clone-fs-attribution/probe.c', 'EVIDENCE.json': 'docs/external-alpha/clone-fs-attribution/EVIDENCE.json', 'RESULTS.json': 'docs/external-alpha/clone-fs-attribution/RESULTS.json'}, 'docs/external-alpha/clone-fs-attribution/RESULTS.json': {'verify.py': 'docs/external-alpha/clone-fs-attribution/verify.py'}, 'docs/external-alpha/clone-fs-attribution/collect.py': {'PLAN.md': 'docs/external-alpha/clone-fs-attribution/PLAN.md', 'probe.c': 'docs/external-alpha/clone-fs-attribution/probe.c'}, 'docs/external-alpha/clone-fs-attribution/original-materials.json': {'REPLAY.json': 'docs/external-alpha/REPLAY.json', 'preservation.json': 'docs/external-alpha/preservation.json', 'verify.py': 'docs/external-alpha/clone-fs-attribution/verify.py'}, 'docs/external-alpha/clone-fs-attribution/verify.py': {'original-materials.json': 'docs/external-alpha/clone-fs-attribution/original-materials.json', 'PLAN.md': 'docs/external-alpha/clone-fs-attribution/PLAN.md', 'probe.c': 'docs/external-alpha/clone-fs-attribution/probe.c', 'collect.py': 'docs/external-alpha/clone-fs-attribution/collect.py', 'RESULTS.json': 'docs/external-alpha/clone-fs-attribution/RESULTS.json', 'EVIDENCE.json': 'docs/external-alpha/clone-fs-attribution/EVIDENCE.json', 'verify.py': 'docs/external-alpha/clone-fs-attribution/verify.py'}, 'docs/external-alpha/meson-16136/RESULTS.md': {'summary.json': 'docs/external-alpha/meson-16136/summary.json', 'evidence-excerpts.json': 'docs/external-alpha/meson-16136/evidence-excerpts.json', '../REPRODUCE.md': 'docs/external-alpha/REPRODUCE.md'}, 'docs/external-alpha/meson-16136/preregistration.sha256': {'CASE.md': 'docs/external-alpha/meson-16136/CASE.md'}, 'docs/external-alpha/pybind11-3445/RESULTS.md': {'summary.json': 'docs/external-alpha/pybind11-3445/summary.json', 'evidence-excerpts.json': 'docs/external-alpha/pybind11-3445/evidence-excerpts.json', '../REPRODUCE.md': 'docs/external-alpha/REPRODUCE.md'}, 'docs/external-alpha/pybind11-3445/preregistration.sha256': {'CASE.md': 'docs/external-alpha/pybind11-3445/CASE.md'}, 'docs/external-alpha/reproduce.py': {'provenance.json': 'docs/external-alpha/provenance.json', 'RETRY.md': 'docs/external-alpha/spades-1339/RETRY.md', 'RETRY-EXACT.md': 'docs/external-alpha/spades-1339/RETRY-EXACT.md'}, 'docs/external-alpha/spades-1339/RESULTS.md': {'summary.json': 'docs/external-alpha/spades-1339/summary.json', 'evidence-excerpts.json': 'docs/external-alpha/spades-1339/evidence-excerpts.json', '../REPRODUCE.md': 'docs/external-alpha/REPRODUCE.md', 'REPLAY.json': 'docs/external-alpha/REPLAY.json', '../REPLAY.json': 'docs/external-alpha/REPLAY.json'}, 'docs/external-alpha/spades-1339/preregistration.sha256': {'CASE.md': 'docs/external-alpha/spades-1339/CASE.md'}, 'docs/external-alpha/verify.py': {'REPRODUCE.md': 'docs/external-alpha/REPRODUCE.md', 'provenance.json': 'docs/external-alpha/provenance.json', 'preservation.json': 'docs/external-alpha/preservation.json', 'CHECKS.md': 'docs/external-alpha/CHECKS.md', 'REPLAY.json': 'docs/external-alpha/REPLAY.json', 'reproduce.py': 'docs/external-alpha/reproduce.py', 'verify.py': 'docs/external-alpha/verify.py', 'spades-1339/RETRY.md': 'docs/external-alpha/spades-1339/RETRY.md', 'spades-1339/RETRY-EXACT.md': 'docs/external-alpha/spades-1339/RETRY-EXACT.md', 'spades-1339/attempts.json': 'docs/external-alpha/spades-1339/attempts.json'}, 'docs/external-cases/pybind11-5626/FROZEN.md': {'freeze-manifest.json': 'docs/external-cases/pybind11-5626/freeze-manifest.json'}, 'docs/external-cases/pybind11-5626/NOISE_ANALYSIS.md': {'repeat-differences.csv': 'docs/external-cases/pybind11-5626/repeat-differences.csv', 'parent-processes.csv': 'docs/external-cases/pybind11-5626/parent-processes.csv', 'summarize.py': 'docs/external-cases/pybind11-5626/summarize.py', 'noise-summary.json': 'docs/external-cases/pybind11-5626/noise-summary.json', 'verify.py': 'docs/external-cases/pybind11-5626/verify.py'}, 'docs/external-cases/pybind11-5626/README.md': {'FROZEN.md': 'docs/external-cases/pybind11-5626/FROZEN.md', 'REPORT.md': 'docs/external-cases/pybind11-5626/REPORT.md', 'NOISE_ANALYSIS.md': 'docs/external-cases/pybind11-5626/NOISE_ANALYSIS.md', 'evidence.json': 'docs/external-cases/pybind11-5626/evidence.json', 'repeat-differences.csv': 'docs/external-cases/pybind11-5626/repeat-differences.csv', 'parent-processes.csv': 'docs/external-cases/pybind11-5626/parent-processes.csv', 'noise-summary.json': 'docs/external-cases/pybind11-5626/noise-summary.json', 'reproduce.py': 'docs/external-cases/pybind11-5626/reproduce.py'}, 'docs/external-cases/pybind11-5626/REPORT.md': {'evidence.json': 'docs/external-cases/pybind11-5626/evidence.json'}, 'docs/external-cases/pybind11-5626/freeze-manifest.json': {'FROZEN.md': 'docs/external-cases/pybind11-5626/FROZEN.md', 'NOISE_ANALYSIS.md': 'docs/external-cases/pybind11-5626/NOISE_ANALYSIS.md', 'REPORT.md': 'docs/external-cases/pybind11-5626/REPORT.md', 'evidence.json': 'docs/external-cases/pybind11-5626/evidence.json', 'noise-summary.json': 'docs/external-cases/pybind11-5626/noise-summary.json', 'parent-processes.csv': 'docs/external-cases/pybind11-5626/parent-processes.csv', 'repeat-differences.csv': 'docs/external-cases/pybind11-5626/repeat-differences.csv', 'reproduce.py': 'docs/external-cases/pybind11-5626/reproduce.py', 'summarize.py': 'docs/external-cases/pybind11-5626/summarize.py', 'verify.py': 'docs/external-cases/pybind11-5626/verify.py'}, 'docs/external-cases/pybind11-5626/reproduce.py': {'evidence.json': 'docs/external-cases/pybind11-5626/evidence.json'}, 'docs/external-cases/pybind11-5626/summarize.py': {'repeat-differences.csv': 'docs/external-cases/pybind11-5626/repeat-differences.csv', 'parent-processes.csv': 'docs/external-cases/pybind11-5626/parent-processes.csv'}, 'docs/external-cases/pybind11-5626/verify.py': {'freeze-manifest.json': 'docs/external-cases/pybind11-5626/freeze-manifest.json', 'noise-summary.json': 'docs/external-cases/pybind11-5626/noise-summary.json', 'evidence.json': 'docs/external-cases/pybind11-5626/evidence.json'}, 'docs/noise-folding-evaluation/README.md': {'EXPECTED_CLUES.md': 'docs/noise-folding-evaluation/EXPECTED_CLUES.md', 'REPRODUCE.md': 'docs/noise-folding-evaluation/REPRODUCE.md', 'RULES.md': 'docs/noise-folding-evaluation/RULES.md', 'RESULTS.json': 'docs/noise-folding-evaluation/RESULTS.json', 'EVIDENCE.json': 'docs/noise-folding-evaluation/EVIDENCE.json', 'collect.py': 'docs/noise-folding-evaluation/collect.py', 'view.py': 'docs/noise-folding-evaluation/view.py', 'audit.py': 'docs/noise-folding-evaluation/audit.py', 'test_view.py': 'docs/noise-folding-evaluation/test_view.py'}, 'docs/noise-folding-evaluation/REPRODUCE.md': {'collect.py': 'docs/noise-folding-evaluation/collect.py', 'docs/noise-folding-evaluation/': 'docs/noise-folding-evaluation/', 'test_view.py': 'docs/noise-folding-evaluation/test_view.py', 'view.py': 'docs/noise-folding-evaluation/view.py', 'audit.py': 'docs/noise-folding-evaluation/audit.py', 'RESULTS.json': 'docs/noise-folding-evaluation/RESULTS.json', 'EVIDENCE.json': 'docs/noise-folding-evaluation/EVIDENCE.json'}, 'docs/noise-folding-evaluation/RESULTS.json': {'view.py': 'docs/noise-folding-evaluation/view.py', 'audit.py': 'docs/noise-folding-evaluation/audit.py', 'test_view.py': 'docs/noise-folding-evaluation/test_view.py', 'collect.py': 'docs/noise-folding-evaluation/collect.py'}, 'docs/noise-folding-evaluation/RULES.md': {'view.py': 'docs/noise-folding-evaluation/view.py', 'EVIDENCE.json': 'docs/noise-folding-evaluation/EVIDENCE.json'}, 'docs/noise-folding-evaluation/audit.py': {'EXPECTED_CLUES.md': 'docs/noise-folding-evaluation/EXPECTED_CLUES.md', 'view.py': 'docs/noise-folding-evaluation/view.py', 'audit.py': 'docs/noise-folding-evaluation/audit.py', 'test_view.py': 'docs/noise-folding-evaluation/test_view.py', 'collect.py': 'docs/noise-folding-evaluation/collect.py'}, 'docs/noise-folding-evaluation/collect.py': {'EXPECTED_CLUES.md': 'docs/noise-folding-evaluation/EXPECTED_CLUES.md'}, 'docs/packaging/PACKAGE_POLICY.md': {'REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json', '../REGRESSION_SET_PUBLIC.json': 'docs/REGRESSION_SET_PUBLIC.json', '../REFACTOR_STATUS.md': 'docs/REFACTOR_STATUS.md', '../CRATE_README.md': 'docs/CRATE_README.md', 'ACCEPTANCE-before-reliability.md': 'docs/ACCEPTANCE-before-reliability.md', 'FIELD_CASES.md': 'docs/FIELD_CASES.md', 'FIELD_CASES_RESULTS.json': 'docs/FIELD_CASES_RESULTS.json', 'HOLDOUT_RESULTS.json': 'docs/HOLDOUT_RESULTS.json', 'RANKING_REPAIR.md': 'docs/RANKING_REPAIR.md', 'RANKING_REPAIR_RESULTS.json': 'docs/RANKING_REPAIR_RESULTS.json', 'BUILD_PATHS.md': 'docs/packaging/BUILD_PATHS.md'}, 'docs/performance/a3-completion-check/MEASUREMENTS.md': {'RESULTS.json': 'docs/performance/a3-completion-check/RESULTS.json'}, 'docs/performance/a3-completion-check/OPTIMIZATION.md': {'optimization/SEMANTICS.json': 'docs/performance/a3-completion-check/optimization/SEMANTICS.json', 'measure.py': 'docs/performance/a3-completion-check/measure.py', 'compare.py': 'docs/performance/a3-completion-check/compare.py', 'RESULTS.json': 'docs/performance/a3-completion-check/RESULTS.json', 'PROBE_FAILURES.json': 'docs/performance/a3-completion-check/optimization/PROBE_FAILURES.json', 'PILOT_DEV_AFTER.json': 'docs/performance/a3-completion-check/optimization/PILOT_DEV_AFTER.json', 'RECORD_CHECKS.json': 'docs/performance/a3-completion-check/RECORD_CHECKS.json', 'FIELD_REPLAY.json': 'docs/performance/a3-completion-check/optimization/FIELD_REPLAY.json', '../../ACCEPTANCE.md': 'docs/ACCEPTANCE.md', 'AUDIT_CLOSURE.json': 'docs/performance/a3-completion-check/optimization/AUDIT_CLOSURE.json', 'optimization/AUDIT_CLOSURE.json': 'docs/performance/a3-completion-check/optimization/AUDIT_CLOSURE.json', 'TEST_RESULTS.json': 'docs/performance/a3-completion-check/optimization/TEST_RESULTS.json'}, 'docs/performance/a3-completion-check/README.md': {'OPTIMIZATION.md': 'docs/performance/a3-completion-check/OPTIMIZATION.md', 'RECORD_CHECKS.json': 'docs/performance/a3-completion-check/RECORD_CHECKS.json', 'MEASUREMENTS.md': 'docs/performance/a3-completion-check/MEASUREMENTS.md', 'RESULTS.json': 'docs/performance/a3-completion-check/RESULTS.json', 'CALIBRATION.json': 'docs/performance/a3-completion-check/CALIBRATION.json', 'DESIGN.md': 'docs/performance/a3-completion-check/DESIGN.md', 'measure.py': 'docs/performance/a3-completion-check/measure.py', 'PLAN.md': 'docs/performance/a3-completion-check/PLAN.md'}, 'docs/performance/a3-completion-check/RESULTS.json': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py'}, 'docs/performance/a3-completion-check/calibrate.py': {'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py'}, 'docs/performance/a3-completion-check/compare.py': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'compare.py': 'docs/performance/a3-completion-check/compare.py', 'PLAN.md': 'docs/performance/a3-completion-check/PLAN.md', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py'}, 'docs/performance/a3-completion-check/measure.py': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py', 'PLAN.md': 'docs/performance/a3-completion-check/PLAN.md', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py'}, 'docs/performance/a3-completion-check/optimization/DEV_AFTER.json': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py'}, 'docs/performance/a3-completion-check/optimization/PILOT_DEV_AFTER.json': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py'}, 'docs/performance/a3-completion-check/optimization/PROBE_FAILURES.json': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py'}, 'docs/performance/a3-completion-check/optimization/RELEASE_AFTER.json': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py'}, 'docs/performance/a3-completion-check/optimization/RELEASE_BEFORE.json': {'measure.py': 'docs/performance/a3-completion-check/measure.py', 'probe_gdb.py': 'docs/performance/a3-completion-check/probe_gdb.py', 'synthetic_backend.py': 'docs/performance/a3-completion-check/synthetic_backend.py'}, 'docs/performance/a3-completion-check/verify.py': {'RESULTS.json': 'docs/performance/a3-completion-check/RESULTS.json', 'PLAN.md': 'docs/performance/a3-completion-check/PLAN.md', 'CALIBRATION.json': 'docs/performance/a3-completion-check/CALIBRATION.json'}, 'docs/performance/a3-completion-check/verify_optimization.py': {'RESULTS.json': 'docs/performance/a3-completion-check/RESULTS.json', 'compare.py': 'docs/performance/a3-completion-check/compare.py', 'optimization/PROBE_FAILURES.json': 'docs/performance/a3-completion-check/optimization/PROBE_FAILURES.json', 'optimization/RECORD_CHECKS.json': 'docs/performance/a3-completion-check/optimization/RECORD_CHECKS.json', 'optimization/SEMANTICS.json': 'docs/performance/a3-completion-check/optimization/SEMANTICS.json', 'optimization/FIELD_REPLAY.json': 'docs/performance/a3-completion-check/optimization/FIELD_REPLAY.json', 'optimization/TEST_RESULTS.json': 'docs/performance/a3-completion-check/optimization/TEST_RESULTS.json', 'optimization/AUDIT_CLOSURE.json': 'docs/performance/a3-completion-check/optimization/AUDIT_CLOSURE.json'}, 'src/main.rs': {'../tests/fixtures/contract_baseline/golden/show.json': 'tests/fixtures/contract_baseline/golden/show.json'}, 'src/parse/mod.rs': {'../../tests/fixtures/trace.txt': 'tests/fixtures/trace.txt'}, 'tests/check_release_closure.py': {'REFACTOR_DECISIONS.md': 'docs/REFACTOR_DECISIONS.md'}, 'tests/contract_baseline.rs': {'fixtures/contract_baseline/golden/list.json': 'tests/fixtures/contract_baseline/golden/list.json', 'fixtures/contract_baseline/golden/list.txt': 'tests/fixtures/contract_baseline/golden/list.txt', 'fixtures/contract_baseline/golden/show.json': 'tests/fixtures/contract_baseline/golden/show.json', 'fixtures/contract_baseline/golden/show.txt': 'tests/fixtures/contract_baseline/golden/show.txt', 'fixtures/contract_baseline/golden/diff_0.txt': 'tests/fixtures/contract_baseline/golden/diff_0.txt', 'fixtures/contract_baseline/golden/diff_0.json': 'tests/fixtures/contract_baseline/golden/diff_0.json', 'fixtures/contract_baseline/golden/diff_1.txt': 'tests/fixtures/contract_baseline/golden/diff_1.txt', 'fixtures/contract_baseline/golden/diff_1.json': 'tests/fixtures/contract_baseline/golden/diff_1.json', 'fixtures/contract_baseline/golden/diff_2.txt': 'tests/fixtures/contract_baseline/golden/diff_2.txt', 'fixtures/contract_baseline/golden/diff_2.json': 'tests/fixtures/contract_baseline/golden/diff_2.json', 'fixtures/contract_baseline/golden/diff_3.txt': 'tests/fixtures/contract_baseline/golden/diff_3.txt', 'fixtures/contract_baseline/golden/diff_3.json': 'tests/fixtures/contract_baseline/golden/diff_3.json'}, 'tests/fixtures/contract_baseline/README.md': {'baseline_manifest.json': 'tests/fixtures/contract_baseline/baseline_manifest.json'}, 'tests/fixtures/contract_baseline/storage/runs/abc001-101/events.jsonl': {'raw/trace.log': 'tests/fixtures/contract_baseline/storage/runs/abc001-101/raw/trace.log'}, 'tests/fixtures/contract_baseline/storage/runs/abc002-102/events.jsonl': {'raw/trace.log': 'tests/fixtures/contract_baseline/storage/runs/abc002-102/raw/trace.log'}, 'tests/fixtures/contract_baseline/storage/runs/abc101-201/events.jsonl': {'raw/trace.log': 'tests/fixtures/contract_baseline/storage/runs/abc101-201/raw/trace.log'}, 'tests/fixtures/contract_baseline/storage/runs/abc102-202/events.jsonl': {'raw/trace.log': 'tests/fixtures/contract_baseline/storage/runs/abc102-202/raw/trace.log'}, 'tests/full_gate.py': {'acceptance_support.py': 'tests/acceptance_support.py'}, 'tests/test_export_safety.py': {'docs/': 'docs/'}, 'tests/test_field_warning_contract.py': {'fixtures/field_report_audit_v1.py': 'tests/fixtures/field_report_audit_v1.py'}, 'tests/test_release_closure.py': {}}
# Filled only for explicitly reviewed synthetic operands and retained public
# documents. A byte change revokes this path-only allowance; credentials and
# caller-supplied private prefixes are scanned even when bytes match.
REVIEWED_PATH_CONTENT = {'docs/ACCEPTANCE.md': 'b98b760bb520f01320a07e213618b4f4ba621931d61f18852bad50f5bec6220a', 'docs/ARCHITECTURE_REVIEW.md': '842259f364d01918e87db0d5ea07f3329d2ca235bad3a0a0f519cf08f4344b2c', 'docs/FIELD_AUDIT_LAYERS.md': '59c5c8e3e450b312c0b075b676add1ff7cc52ac208d9ddd07fddb39503d04e6b', 'docs/FIELD_CASES.md': '3cead615accf7784e45a8cde7267d626e94a10a2ca4cbc25db00ab8f87561b67', 'docs/FIELD_CASES_RESULTS.json': '132d7439f2b8c85866c43f33a7698fdd9a7e858a7389e0f2c3fdacbb58f75ea7', 'docs/RANKING_REPAIR.md': 'c73ca8034e61b657318b34d3bf46ddbccd867d6eba6a4cc64c7f606b5f4b71f1', 'docs/external-alpha/batch-2/CHECKS.md': '22cc2adadb32e9a46587a708bcdc88945c4ffa9658e626b1dbbaa96ad7f758ad', 'docs/external-alpha/batch-2/pip-12996/CASE.md': 'd46bc440f649413d3dc61362465d2b4c9b82f069dd26619fab1247b640b5cbb9', 'docs/external-alpha/batch-2/pip-12996/RESULTS.md': 'eb19f64609db37382615d8023d3f7425e7b33ae8666f41f1c6320cdf6475d94b', 'docs/external-alpha/batch-2/pip-12996/evidence.json': 'd2097d82c64463ffb8601a36d93c3315990f98b566c108298a0bed6447912dfa', 'docs/external-alpha/batch-2/pip-12996/summary.json': '4f282fb2972d745e56c90d45806a9939936515711342354f78fe2e7f7116e3c0', 'docs/external-alpha/batch-2/test_redaction.py': '1fd9731851c7d98ae3f1e1b2be4531e3afa18bb45873ca14ead524061751da37', 'docs/packaging/check_package.py': 'b6ac0d29165b4b0377c48f2b9aa3508f32d8e50c59f73ab8009de660e266e272', 'docs/performance/a3-completion-check/compare.py': '6506730c18d190444d73bae64347b798e981a35cc0af3818ec579bbf7e3b3012', 'docs/performance/a3-completion-check/verify.py': '69ad35ee3c4b37db95ab7284536d2b8b3472a1f2e757e55431ac9a301ab475bb', 'docs/performance/a3-completion-check/verify_optimization.py': 'b3f1356fcf7c362f0641f8123e03319b4121d73740c135f063174a45772c7302', 'examples/field_cases.py': '3729e9e68b8e3942104a40b98f2cf1ddfe5aa19f59239ef62d3f9cb32ea1c795', 'tests/check_public_archive_freeze.py': '08e80d38a13587b380b5070dbfc4b85d50e10ad2385fd4e11f9d40f028e5554f', 'tests/contract_baseline.rs': '15306a05301f721ed560a6821da786c1d53d07e41cc422803bed0ec292f994df', 'tests/full_gate.py': 'b37235d20e738b74485eeec03d14a9b30cb8b5fae58f2d14e72c479bd647f29f', 'tests/test_public_archive_freeze.py': 'd95899c29d5da72af5a6a96080e5c4f094131fc8e2bf1ae916d7857abe20be52'}

SYNTHETIC_MATERIAL_CONTEXTS = {'.gitignore': [('/.idea/\n', '/.idea/')], 'docs/packaging/check_package.py': [("    for name in ('.idea/private.xml', 'target/program', 'docs/external-alpha/evidence.json',\n                 'tests/fixtures/trace.txt', 'README.md', 'docs/ALPHA_READINESS.md'):\n        reject(lambda name=name: validate(valid | {name: b'synthetic'}))\n", '.idea/private.xml'), ("    for name in ('.idea/private.xml', 'target/program', 'docs/external-alpha/evidence.json',\n                 'tests/fixtures/trace.txt', 'README.md', 'docs/ALPHA_READINESS.md'):\n        reject(lambda name=name: validate(valid | {name: b'synthetic'}))\n", 'docs/external-alpha/evidence.json')], 'tests/test_release_closure.py': [('            b"REFACTOR_DECISIONS.md\\n"', 'REFACTOR_DECISIONS.md')], 'tests/test_export_safety.py': [('        for value, rule in (\n            (b"docs/FIELD_CASES-private.md", "excluded_material"),\n            (b"docs/external-alpha/unknown-record.json", "excluded_material"),\n            (b"unapproved/FIELD_CASES.md", "excluded_material"),\n            (b"unapproved/docs/FIELD_CASES.md", "excluded_material"),\n            (("/" + "home/example/private").encode(), "private_path"),\n        ):', 'docs/FIELD_CASES-private.md'), ('        for value, rule in (\n            (b"docs/FIELD_CASES-private.md", "excluded_material"),\n            (b"docs/external-alpha/unknown-record.json", "excluded_material"),\n            (b"unapproved/FIELD_CASES.md", "excluded_material"),\n            (b"unapproved/docs/FIELD_CASES.md", "excluded_material"),\n            (("/" + "home/example/private").encode(), "private_path"),\n        ):', 'docs/external-alpha/unknown-record.json'), ('        for value, rule in (\n            (b"docs/FIELD_CASES-private.md", "excluded_material"),\n            (b"docs/external-alpha/unknown-record.json", "excluded_material"),\n            (b"unapproved/FIELD_CASES.md", "excluded_material"),\n            (b"unapproved/docs/FIELD_CASES.md", "excluded_material"),\n            (("/" + "home/example/private").encode(), "private_path"),\n        ):', 'unapproved/FIELD_CASES.md'), ('        for value, rule in (\n            (b"docs/FIELD_CASES-private.md", "excluded_material"),\n            (b"docs/external-alpha/unknown-record.json", "excluded_material"),\n            (b"unapproved/FIELD_CASES.md", "excluded_material"),\n            (b"unapproved/docs/FIELD_CASES.md", "excluded_material"),\n            (("/" + "home/example/private").encode(), "private_path"),\n        ):', 'unapproved/docs/FIELD_CASES.md')]}
SYNTHETIC_PATH_CONTEXTS = {'tests/fresh_field_contract.py': [("                         'git-identity/home/.gitconfig', ['success'], ['ENOENT'], 'root', None),", 'git-identity/home/.gitconfig')]}


def decode_escapes(text):
    text = text.replace(r"\/", "/").replace("\\\\", "\\").replace(r'\"', '"')

    def char(match):
        value = match[1]
        code = int(value[1:], 16) if value[0] in "xuU" else int(value, 8)
        return chr(code) if code <= 0x10FFFF else "?"

    text = re.sub(r"(?<!\\)\\(x[0-9a-fA-F]{2}|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|[0-7]{1,3})", char, text)
    return html.unescape(urllib.parse.unquote(text))


def mask_ranges(text, ranges):
    """Replace only approved original lexical ranges before any decoding."""
    merged = []
    for start, end in sorted(set(ranges)):
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(end, merged[-1][1]))
        else:
            merged.append((start, end))
    for start, end in reversed(merged):
        text = text[:start] + "reviewed-syntax" + text[end:]
    return text


def reference_context(name, inventory):
    """Prepare exact source aliases once; no state escapes a scan invocation."""
    targets = inventory | {str(PurePosixPath(path).parent) + "/" for path in inventory}
    aliases = {alias for alias, target in SOURCE_REFERENCES.get(name, {}).items() if target in targets}
    return frozenset(inventory | aliases)


def valid_reference(value, name, inventory, *, references=None):
    if not isinstance(value, str) or value.startswith("/"):
        return False
    if references is not None:
        return value in references
    if value in inventory:
        return True
    target = SOURCE_REFERENCES.get(name, {}).get(value)
    if target is None:
        return False
    directories = {str(PurePosixPath(path).parent) + "/" for path in inventory}
    return target in inventory or target in directories


def toml_material_ranges(text, payload, readme):
    """Authorize complete scalars in package.include/readme, never free text."""
    try:
        package = tomllib.loads(text)["package"]
    except (ValueError, KeyError, TypeError):
        return []
    ranges = []
    section = re.search(r"(?ms)^\[package\]\s*$(.*?)(?=^\[|\Z)", text)
    if section is None:
        return ranges
    offset = section.start(1)
    body = section[1]
    quoted = r'"(?:\\.|[^"\\])*"|\'[^\']*\''
    expected = {"/" + path for path in payload}
    includes = package.get("include")
    if isinstance(includes, list) and len(includes) == len(expected) and all(isinstance(value, str) for value in includes) and set(includes) == expected:
        for array in re.finditer(r"(?ms)^include\s*=\s*\[(.*?)\]", body):
            for token in re.finditer(quoted, array[1]):
                try:
                    value = tomllib.loads("value = " + token[0])["value"]
                except ValueError:
                    continue
                if value in expected:
                    start = offset + array.start(1) + token.start()
                    ranges.append((start, start + len(token[0])))
    if package.get("readme") == readme:
        for field in re.finditer(r"(?m)^readme\s*=\s*(" + quoted + r")", body):
            if tomllib.loads("value = " + field[1])["value"] == readme:
                ranges.append((offset + field.start(1), offset + field.end(1)))
    return ranges


PYTHON_NEGATIVE_VALUES = {
    "tests/test_export_safety.py": {
        "test_repo_and_crate_inventories_are_distinct_and_exact": {
            "docs/README_MERGE_PROPOSAL.md", "/docs/FIELD_CASES.md"},
    },
}


SYNTHETIC_JOINED_VALUES = {'tests/test_release_closure.py': {'REFACTOR_DECISIONS.md\nperformance/rf600-streaming/README.md\ntests/full_gate.py\n'}}


def reference_tokens(value):
    return re.finditer(r"(?<![A-Za-z0-9_./\\-])[A-Za-z0-9_.][A-Za-z0-9_./-]*(?![A-Za-z0-9_./\\-])", value)


SCOPED_PYTHON_TARGETS = {
    "examples/holdout_cases.py": {"docs/HOLDOUT_PLAN.md"},
    "tests/check_regression_freeze.py": {"docs/REGRESSION_SET.json"},
    "tests/full_gate.py": {"docs/packaging/check_package.py"},
}

GITIGNORE_PATTERN_SHA256 = "19234b4d064ab3135f9039bf9b120ecb9f02770c60ab9dc725a69b07f2942656"


def scoped_python_material(node, name, tree, parents, pieces):
    """Admit only the reviewed pathlib roles and the exact ignore-parser regex."""
    def same(left, spelling):
        return ast.dump(left, include_attributes=False) == ast.dump(ast.parse(spelling, mode="eval").body, include_attributes=False)
    def ancestors(value):
        while value in parents:
            value = parents[value]
            yield value
    def function(value):
        return next((owner for owner in ancestors(value) if isinstance(owner, (ast.FunctionDef, ast.AsyncFunctionDef))), None)
    def assignments(scope, label):
        return [item for item in ast.walk(scope) if isinstance(item, ast.Assign) and
                any(isinstance(target, ast.Name) and target.id == label for target in item.targets)]
    def module_root():
        roots = [item for item in tree.body if isinstance(item, ast.Assign) and
                 any(isinstance(target, ast.Name) and target.id == "ROOT" for target in item.targets)]
        rebound_path = any(isinstance(node, ast.Name) and node.id == "Path" and isinstance(node.ctx, ast.Store)
                           for statement in tree.body if not isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
                           for node in ast.walk(statement))
        return not rebound_path and len(roots) == 1 and same(roots[0].value, "Path(__file__).resolve().parents[1]") and any(
            isinstance(item, ast.ImportFrom) and item.module == "pathlib" and
            any(alias.name == "Path" and alias.asname is None for alias in item.names) for item in tree.body)
    def unshadowed_root(scope):
        return module_root() and not any(
            (isinstance(item, ast.Name) and item.id == "ROOT" and isinstance(item.ctx, ast.Store)) or
            (isinstance(item, ast.arg) and item.arg == "ROOT") or
            (isinstance(item, ast.Global) and "ROOT" in item.names) for item in ast.walk(scope))
    if len(pieces) != 1 or not isinstance(node.value, str):
        return False
    parent = parents.get(node)
    owner_function = function(node)
    if (name == "tools/check_export.py" and owner_function is not None and
            owner_function in tree.body and owner_function.name == "original_ranges" and
            hashlib.sha256(node.value.encode()).hexdigest() == GITIGNORE_PATTERN_SHA256):
        branch = next((owner for owner in ancestors(node) if isinstance(owner, ast.If) and
                       same(owner.test, "name == '.gitignore'")), None)
        return (branch is not None and isinstance(parent, ast.Call) and
                same(parent.func, "re.finditer") and len(parent.args) == 2 and
                parent.args[0] is node and same(parent.args[1], "text") and not parent.keywords and
                any(parent is item for statement in branch.body for item in ast.walk(statement)))
    if (not isinstance(parent, ast.BinOp) or not isinstance(parent.op, ast.Div) or
            parent.right is not node or not isinstance(parent.left, ast.Name)):
        return False
    if name == "examples/holdout_cases.py" and node.value == "docs/HOLDOUT_PLAN.md" and parent.left.id == "ROOT" and module_root():
        assignment = next((owner for owner in ancestors(parent) if isinstance(owner, ast.Assign)), None)
        return (assignment in tree.body and len(assignment.targets) == 1 and
                isinstance(assignment.targets[0], ast.Name) and assignment.targets[0].id == "FROZEN" and
                isinstance(assignment.value, ast.DictComp) and isinstance(parents.get(parent), ast.List))
    if name == "tests/check_regression_freeze.py" and node.value == "docs/REGRESSION_SET.json" and parent.left.id == "ROOT" and module_root():
        assignment = next((owner for owner in ancestors(parent) if isinstance(owner, ast.Assign)), None)
        return (assignment in tree.body and len(assignment.targets) == 1 and
                isinstance(assignment.targets[0], ast.Name) and assignment.targets[0].id == "manifest" and
                same(assignment.value, "json.loads((ROOT / 'docs/REGRESSION_SET.json').read_text())"))
    if name != "tests/full_gate.py" or node.value != "docs/packaging/check_package.py" or owner_function is None or owner_function not in tree.body:
        return False
    assignment = parents.get(parent)
    if (not isinstance(assignment, ast.Assign) or assignment.value is not parent or
            len(assignment.targets) != 1 or not isinstance(assignment.targets[0], ast.Name) or
            assignment.targets[0].id != "checker" or len(assignments(owner_function, "checker")) != 1):
        return False
    if owner_function.name == "package_optimization_controls" and parent.left.id == "ROOT":
        return unshadowed_root(owner_function)
    if owner_function.name != "package_gate" or parent.left.id != "source":
        return False
    sources, works = assignments(owner_function, "source"), assignments(owner_function, "work")
    if len(sources) != 1 or len(works) != 1 or not same(sources[0].value, "work / 'source'") or not same(works[0].value, "Path(directory)"):
        return False
    scope = next((owner for owner in ancestors(sources[0]) if isinstance(owner, ast.With)), None)
    if scope is None or len(scope.items) != 1 or not same(scope.items[0].context_expr, "tempfile.TemporaryDirectory(prefix='rundelta-full-gate-')") or not isinstance(scope.items[0].optional_vars, ast.Name) or scope.items[0].optional_vars.id != "directory":
        return False
    if any(sum(isinstance(item, ast.Name) and isinstance(item.ctx, ast.Store) and item.id == label
               for item in ast.walk(owner_function)) != 1 for label in ("source", "work", "directory")):
        return False
    extracts = [item for item in ast.walk(scope) if isinstance(item, ast.Call) and
                same(item, "extract_clean_tree(exported, source)") and item.lineno < assignment.lineno]
    return len(extracts) == 1 and sources[0].lineno < extracts[0].lineno and works[0].lineno < sources[0].lineno


def python_ranges(name, text, inventory, contexts, *, paths=False, references=None, reference_cache=None):
    """Use AST string nodes and roles; copied/decoded prose is not a policy."""
    try:
        tree = ast.parse(text)
    except (SyntaxError, ValueError):
        return []
    lines = text.splitlines(keepends=True)
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line))
    parents = {child: node for node in ast.walk(tree) for child in ast.iter_child_nodes(node)}
    references = reference_context(name, inventory) if references is None else references
    reference_cache = {} if reference_cache is None else reference_cache
    def is_reference(value):
        if value not in reference_cache:
            reference_cache[value] = valid_reference(value, name, inventory, references=references)
        return reference_cache[value]
    def position(line, byte_column):
        return offsets[line - 1] + len(lines[line - 1].encode()[:byte_column].decode())
    def bounds(node):
        return position(node.lineno, node.col_offset), position(node.end_lineno, node.end_col_offset)
    def function(node):
        while node in parents:
            node = parents[node]
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                return node.name
        return None
    approved_contexts = []
    for context, operand in contexts.get(name, ()):
        start = 0
        while (start := text.find(context, start)) >= 0:
            approved_contexts.append((start, start + len(context), operand))
            start += len(context)
    policy_nodes = set()
    if name == "tools/check_export.py":
        # Validate complete typed policy declarations before marking their AST
        # scalar nodes. Other statements in the same file gain no permission.
        for assignment in tree.body:
            if not isinstance(assignment, ast.Assign) or len(assignment.targets) != 1 or not isinstance(assignment.targets[0], ast.Name):
                continue
            label = assignment.targets[0].id
            try:
                value = ast.literal_eval(assignment.value)
            except (ValueError, TypeError):
                continue
            valid = False
            if label == "SOURCE_REFERENCES" and isinstance(value, dict):
                policy_contexts = {source: reference_context(source, inventory) for source in value if source in REPO_FILES}
                valid = all(source in REPO_FILES and isinstance(aliases, dict) and
                            all(isinstance(alias, str) and valid_reference(alias, source, inventory, references=policy_contexts[source]) and
                                SOURCE_REFERENCES.get(source, {}).get(alias) == target
                                for alias, target in aliases.items()) for source, aliases in value.items())
            elif label in ("SYNTHETIC_MATERIAL_CONTEXTS", "SYNTHETIC_PATH_CONTEXTS", "PYTHON_NEGATIVE_VALUES", "SYNTHETIC_JOINED_VALUES"):
                valid = value == globals()[label]
            if valid:
                policy_nodes.update(ast.walk(assignment.value))
    lexical = []
    for token in tokenize.generate_tokens(io.StringIO(text).readline):
        if token.type == tokenize.STRING:
            start = offsets[token.start[0] - 1] + token.start[1]
            end = offsets[token.end[0] - 1] + token.end[1]
            lexical.append((start, end, token.string))
    result = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Constant) or not isinstance(node.value, (str, bytes)):
            continue
        value = node.value.decode("utf-8", errors="replace") if isinstance(node.value, bytes) else node.value
        start, end = bounds(node)
        parent = parents.get(node)
        negative = False
        if value in PYTHON_NEGATIVE_VALUES.get(name, {}).get(function(node), set()):
            if isinstance(parent, ast.Call) and isinstance(parent.func, ast.Attribute):
                negative = parent.func.attr == "assertNotIn" and bool(parent.args) and parent.args[0] is node
            elif isinstance(parent, ast.Tuple):
                owner = parents.get(parent)
                negative = isinstance(owner, ast.For) and isinstance(owner.target, ast.Name) and owner.target.id == "added"
        if node in policy_nodes or (not paths and negative):
            result.append((start, end))
            continue
        pieces = [(a, b, token) for a, b, token in lexical if start <= a and b <= end]
        if not paths and (value in inventory or hashlib.sha256(value.encode()).hexdigest() == GITIGNORE_PATTERN_SHA256) and scoped_python_material(node, name, tree, parents, pieces):
            # Target paths must belong to this schema; the regex instead has a
            # pinned complete value and the narrowly checked parser call role.
            result.append((start, end))
            continue
        joined = value in SYNTHETIC_JOINED_VALUES.get(name, set()) and isinstance(parent, ast.Call) and isinstance(parent.func, ast.Attribute) and parent.func.attr == "write"
        approved_values = {match[0] for match in reference_tokens(value)
                           if is_reference(match[0]) and match[0] not in SCOPED_PYTHON_TARGETS.get(name, set())} if not paths and not isinstance(parent, ast.BinOp) else set()
        for token_start, token_end, token in pieces:
            try:
                scalar = ast.literal_eval(token)
            except (ValueError, SyntaxError):
                continue
            scalar = scalar.decode("utf-8", errors="replace") if isinstance(scalar, bytes) else scalar
            if not isinstance(scalar, str):
                continue
            synthetic = (len(pieces) == 1 or joined) and any(
                a <= token_start and token_end <= b and operand in scalar
                for a, b, operand in approved_contexts)
            if synthetic:
                result.append((token_start, token_end))
            elif not paths:
                # Validate the complete decoded AST value first, then mask only
                # the identical literal spelling in this original source token.
                # Adjacent literals cannot launder an unapproved parent path.
                for match in reference_tokens(token):
                    if match[0] in approved_values:
                        result.append((token_start + match.start(), token_start + match.end()))
    return result


def original_ranges(name, text, inventory, payload, readme, *, paths=False, references=None, reference_cache=None):
    if name.endswith(".py"):
        rules = SYNTHETIC_PATH_CONTEXTS if paths else SYNTHETIC_MATERIAL_CONTEXTS
        return python_ranges(name, text, inventory, rules, paths=paths, references=references, reference_cache=reference_cache)
    if paths:
        return []
    if name in ("Cargo.toml", "Cargo.toml.orig"):
        return toml_material_ranges(text, payload, readme)
    if name == ".gitignore":
        return [match.span() for match in re.finditer(r"(?m)^/\.idea/\n", text)]
    result = []
    references = reference_context(name, inventory) if references is None else references
    reference_cache = {} if reference_cache is None else reference_cache
    for token in re.finditer(r"(?<![A-Za-z0-9_./\\-])[A-Za-z0-9_.][A-Za-z0-9_./-]*(?![A-Za-z0-9_./\\-])", text):
        value = token[0]
        if value not in reference_cache:
            reference_cache[value] = valid_reference(value, name, inventory, references=references)
        if reference_cache[value]:
            result.append(token.span())
    return result


# Decoder budgets are per file and shared across rule views and nesting levels.
# Short canonical tokens remain supported; repeated successes and failures cost
# one decode, while occurrence/input caps also bound cache-hit scanning work.
BASE64_MAX_ATTEMPTS = 16_384
BASE64_MAX_INPUT_CHARS = 16_000_000
BASE64_MAX_MATCHES = 1_000_000


def _decode_base64_token(token):
    """Decode one canonical token; None is a cacheable rejection."""
    unpadded = token.rstrip("=")
    if len(unpadded) % 4 == 1 or ("=" in token and len(token) % 4):
        return None
    try:
        value = base64.b64decode(unpadded + "=" * (-len(unpadded) % 4), altchars=b"-_", validate=True)
        canonical = base64.b64encode(value).decode()
        normalized = unpadded.replace("-", "+").replace("_", "/")
        if canonical.rstrip("=") != normalized:
            return None
        if "=" in token and token.replace("-", "+").replace("_", "/") != canonical:
            return None
        decoded = value.decode("utf-8")
    except (ValueError, UnicodeDecodeError):
        return None
    # NUL/control bytes do not invalidate UTF-8 or remove sensitive substrings.
    # Keep the complete decoded view; nesting and work budgets bound its scan.
    return decoded if decoded else None


def privacy(name, data, private_prefixes=(), *, legacy=False, public=False,
            syntax_inventory=None, syntax_payload=None, syntax_readme=None, material=True):
    """Scan separate provenance-preserving views with strict decode budgets."""
    counts = Counter()
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return Counter({"non_utf8_source": 1})
    inventory = syntax_inventory if syntax_inventory is not None else REPO_FILES
    payload = syntax_payload if syntax_payload is not None else CRATE_FILES
    readme = syntax_readme or "docs/CRATE_README.md"
    contextual = public or syntax_inventory is not None
    references = reference_context(name, inventory) if contextual else None
    reference_cache = {}
    path_ranges = original_ranges(name, text, inventory, payload, readme, paths=True,
                                  references=references, reference_cache=reference_cache) if contextual else []
    if contextual and hashlib.sha256(data).hexdigest() == REVIEWED_PATH_CONTENT.get(name):
        # Existing reviewed content contributes original literal ranges only;
        # newly decoded data cannot acquire a whole-file path exemption.
        path_ranges.extend(match.span() for match in PRIVATE_PATH.finditer(text))
    material_ranges = original_ranges(name, text, inventory, payload, readme,
                                      references=references, reference_cache=reference_cache) if contextual else []
    queue = deque([(text, "secrets", 0), (mask_ranges(text, path_ranges), "paths", 0)])
    if material:
        queue.append((mask_ranges(text, material_ranges), "materials", 0))
    seen = set()
    expanded = 0
    base64_cache = {}
    base64_scheduled = set()
    base64_attempts = base64_input_chars = base64_matches = 0
    base64_exhausted = False
    constants = LEGACY_CONSTANTS if legacy else CONSTANTS
    while queue:
        view, role, depth = queue.popleft()
        if (view, role) in seen:
            continue
        expanded += len(view)
        if depth > 12 or len(seen) >= 20_000 or expanded > 32_000_000:
            counts["encoding_budget_exceeded"] += 1
            break
        seen.add((view, role))
        filtered = view
        if role == "paths":
            for value in constants.get(name, ()):
                filtered = re.sub(r'\\*"' + re.escape(value) + r'\\*"', '"reviewed-constant"', filtered)
            counts["private_path"] += len(PRIVATE_PATH.findall(filtered))
        elif role == "materials":
            counts["excluded_material"] += len(MATERIAL.findall(filtered))
        else:
            counts["credential_shape"] += len(SECRET.findall(view))
            counts["explicit_private_prefix"] += sum(view.count(p) for p in private_prefixes if p)
        decoded = decode_escapes(view)
        if decoded != view:
            queue.append((decoded, role, depth + 1))
        for match in WRAPPED_B64.finditer(view):
            queue.append(("".join(match[0].splitlines()), role, depth + 1))
        for match in HEX.finditer(view):
            try:
                queue.append((bytes.fromhex(match[0]).decode("utf-8"), role, depth + 1))
            except UnicodeDecodeError:
                pass
        for match in B64.finditer(view):
            token = match[0]
            base64_matches += 1
            base64_input_chars += len(token)
            if base64_matches > BASE64_MAX_MATCHES:
                counts["base64_match_budget_exceeded"] += 1
                base64_exhausted = True
                break
            if base64_input_chars > BASE64_MAX_INPUT_CHARS:
                counts["base64_input_budget_exceeded"] += 1
                base64_exhausted = True
                break
            if token not in base64_cache:
                if base64_attempts >= BASE64_MAX_ATTEMPTS:
                    counts["base64_attempt_budget_exceeded"] += 1
                    base64_exhausted = True
                    break
                base64_attempts += 1
                base64_cache[token] = _decode_base64_token(token)
            decoded = base64_cache[token]
            # Cache decoding across roles without sharing rule permissions.
            # Schedule each decoded role-view only once, including repeated
            # tokens within a single large view, so the queue also stays bounded.
            key = (decoded, role)
            if decoded is not None and key not in base64_scheduled and key not in seen:
                base64_scheduled.add(key)
                queue.append((decoded, role, depth + 1))
        if base64_exhausted:
            break
    return +counts


def inspect_files(files, expected, private_prefixes=(), *, legacy=False, payload=None, readme=None, public=True):
    findings = {}
    payload = payload if payload is not None else (LEGACY_PAYLOAD if legacy else CRATE_FILES)
    readme = readme or ("README.md" if legacy or payload == HISTORIC_PAYLOAD else "docs/CRATE_README.md")
    for name in sorted(set(files) | expected):
        if name not in files:
            findings[name] = Counter({"missing_allowed_file": 1})
        elif name not in expected:
            findings[name] = Counter({"unexpected_file": 1})
        else:
            counts = privacy(name, files[name], private_prefixes, legacy=legacy,
                             syntax_inventory=expected, syntax_payload=payload, syntax_readme=readme)
            if counts:
                findings[name] = counts
    if files.get("LICENSE") and hashlib.sha256(files["LICENSE"]).hexdigest() != LICENSE_SHA:
        findings.setdefault("LICENSE", Counter())["license_mismatch"] += 1
    for name in ("Cargo.toml", "Cargo.toml.orig"):
        if name not in files:
            continue
        try:
            pkg = tomllib.loads(files[name].decode())["package"]
            valid = (pkg["name"] == "RunDelta" and pkg["version"] == ("0.1.0" if legacy else VERSION)
                     and pkg["license"] == "MIT" and pkg["readme"] == readme
                     and pkg["repository"] == "https://github.com/Dohyeon2006/rundelta"
                     and set(pkg["include"]) == {"/" + n for n in payload}
                     and len(pkg["include"]) == len(payload)
                     and not any(pkg.get(k) for k in ("homepage", "documentation")))
        except (KeyError, TypeError, ValueError):
            valid = False
        if not valid:
            findings.setdefault(name, Counter())["manifest_mismatch"] += 1
    return findings


def git(root, *args):
    return subprocess.check_output(["git", "--no-optional-locks", "--no-replace-objects",
                                    "-C", str(root), *args], stderr=subprocess.PIPE)


def worktree_files(root):
    """Include ignored and untracked files; only Git's own metadata is exempt."""
    files = {}
    for path in root.rglob("*"):
        name = path.relative_to(root).as_posix()
        if name == ".git" or name.startswith(".git/"):
            continue
        if path.is_symlink():
            raise ValueError("symlink")
        if path.is_file():
            files[name] = path.read_bytes()
        elif path.is_dir() and not any(n.startswith(name + "/") for n in REPO_FILES):
            files[name + "/"] = b""
        elif not path.is_dir():
            raise ValueError("nonregular_worktree_entry")
    for name in EXECUTABLE_FILES & files.keys():
        if not (root / name).stat().st_mode & 0o111:
            raise ValueError("worktree_mode_mismatch")
    for name in files.keys() - EXECUTABLE_FILES:
        if name in REPO_FILES and (root / name).is_file() and (root / name).stat().st_mode & 0o111:
            raise ValueError("worktree_mode_mismatch")
    return files


def tree_files(root, revision, *, historical=False):
    files = {}
    for row in git(root, "ls-tree", "-rz", "--full-tree", revision).split(b"\0"):
        if not row:
            continue
        meta, name = row.split(b"\t", 1)
        mode, kind, oid = meta.split()
        expected_mode = b"100755" if not historical and name.decode() in EXECUTABLE_FILES else b"100644"
        if mode != expected_mode or kind != b"blob":
            raise ValueError("unsafe_git_entry")
        files[name.decode()] = git(root, "cat-file", "blob", oid.decode())
    return files


def history_findings(root, private_prefixes=(), *, baseline=BASELINE):
    """Check every ancestor, including deleted blobs and commit metadata.

    Only the already-reviewed public root receives historical format rules.
    The optional baseline parameter is a test seam, not a CLI override.
    """
    grafts = Path(git(root, "rev-parse", "--git-path", "info/grafts").decode().strip())
    if not grafts.is_absolute():
        grafts = root / grafts
    if (grafts.exists() or git(root, "rev-parse", "--is-shallow-repository").strip() != b"false"
            or git(root, "for-each-ref", "refs/replace").strip()):
        raise ValueError("incomplete_or_rewritten_history_view")
    commits = git(root, "rev-list", "--reverse", "HEAD").decode().splitlines()
    if not commits or commits[0] != baseline:
        raise ValueError("unexpected_history_root")
    rows = []
    previous = None
    for commit in commits:
        parents = git(root, "rev-list", "--parents", "-n", "1", commit).decode().split()[1:]
        if parents != ([] if previous is None else [previous]):
            raise ValueError("nonlinear_history")
        schema = HISTORIC_SCHEMAS.get(commit)
        if schema is not None:
            expected_tree, inventory, payload, legacy = schema
            if git(root, "rev-parse", commit + "^{tree}").decode().strip() != expected_tree:
                raise ValueError("historic_tree_mismatch")
        elif commit == baseline and baseline != BASELINE:
            # Synthetic baseline seam; the production CLI cannot select it.
            inventory, payload, legacy = LEGACY_WORKTREE, LEGACY_PAYLOAD, True
        else:
            inventory, payload, legacy = REPO_FILES, CRATE_FILES, False
        historical = schema is not None or legacy
        files = tree_files(root, commit, historical=historical)
        findings = inspect_files(files, inventory, private_prefixes, legacy=legacy,
                                 payload=payload, readme="README.md" if historical else "docs/CRATE_README.md",
                                 public=not historical)
        metadata = privacy("commit-metadata", git(root, "cat-file", "commit", commit), private_prefixes)
        if metadata:
            findings["commit-metadata"] = metadata
        rows.append(findings)
        previous = commit
    return rows


def validate_tar_header(header, member, private_prefixes=()):
    """Validate every byte in the supported short-name GNU/USTAR profiles."""
    if len(header) != 512:
        raise ValueError("unexpected_tar_metadata")
    def string_field(start, end):
        field = header[start:end]
        nul = field.find(b"\0")
        if nul < 0 or any(field[nul:]):
            raise ValueError("unexpected_tar_metadata")
        return field[:nul]
    def octal_field(start, end, *, checksum=False, zero_empty=False):
        field = header[start:end]
        if zero_empty and not any(field):
            return 0
        pattern = rb"(?:[0-7]+\0 |[0-7]+\0)" if checksum else rb"[0-7]+\0"
        if re.fullmatch(pattern, field) is None:
            raise ValueError("unexpected_tar_metadata")
        return int(field.rstrip(b"\0 "), 8)
    prefix = "RunDelta-" + VERSION + "/"
    name = string_field(0, 100).decode("utf-8")
    if not name.startswith(prefix) or name[len(prefix):] not in CRATE_ARCHIVE_FILES or name != member.name:
        raise ValueError("unsafe_archive_entry")
    if header[156:157] != tarfile.REGTYPE or string_field(157, 257):
        raise ValueError("unsafe_archive_entry")
    if header[257:265] not in (b"ustar\00000", b"ustar  \0"):
        raise ValueError("unexpected_tar_metadata")
    if string_field(265, 297) or string_field(297, 329) or any(header[345:512]):
        raise ValueError("unexpected_tar_metadata")
    if (octal_field(100, 108) != 0o644 or octal_field(108, 116, zero_empty=True) != 0 or
            octal_field(116, 124, zero_empty=True) != 0 or octal_field(124, 136) != member.size or
            octal_field(329, 337, zero_empty=True) != 0 or octal_field(337, 345, zero_empty=True) != 0):
        raise ValueError("unexpected_tar_metadata")
    octal_field(136, 148)
    expected = sum(header[:148]) + 8 * 32 + sum(header[156:])
    if octal_field(148, 156, checksum=True) != expected:
        raise ValueError("unexpected_tar_metadata")
    # Only the fully validated member name is exempt from document-material
    # rules. All header bytes still undergo secrets/private-prefix/path scans.
    if privacy("archive-metadata", header, private_prefixes, material=False):
        raise ValueError("private_archive_metadata")


def archive_files(archive, private_prefixes=()):
    # Cargo needs one gzip stream and ordinary short-path tar entries. Its one
    # optional header is the exact package basename, not an arbitrary filename.
    # Reject other metadata, additional streams, and trailers, rather than
    # letting a permissive decompressor silently skip unreviewed bytes.
    if archive.stat().st_size > 8_000_000:
        raise ValueError("oversized_archive")
    compressed = archive.read_bytes()
    if len(compressed) < 10 or compressed[:3] != b"\x1f\x8b\x08" or compressed[3] not in (0, 8):
        raise ValueError("unexpected_gzip_metadata")
    if compressed[3] == 8:
        basename = ("RunDelta-" + VERSION + ".crate").encode() + b"\0"
        if not compressed[10:].startswith(basename):
            raise ValueError("unexpected_gzip_filename")
    decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
    expanded = decoder.decompress(compressed, 32_000_000)
    if not decoder.eof or decoder.unused_data or decoder.unconsumed_tail:
        raise ValueError("unexpected_archive_trailer")
    files = {}
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as handle:
        for member in handle:
            path = PurePosixPath(member.name)
            if (member.type != tarfile.REGTYPE or path.is_absolute() or ".." in path.parts
                    or len(path.parts) < 2 or path.parts[0] != "RunDelta-" + VERSION
                    or member.name != path.as_posix()
                    or member.size > 1_000_000 or len(files) >= 100):
                raise ValueError("unsafe_archive_entry")
            if (handle.pax_headers or member.pax_headers or member.uname or member.gname
                    or member.linkname or member.uid or member.gid
                    or member.mode != 0o644
                    or member.offset_data - member.offset != 512):
                raise ValueError("unexpected_tar_metadata")
            validate_tar_header(expanded[member.offset:member.offset_data], member, private_prefixes)
            end = member.offset_data + member.size
            if any(expanded[end:((end + 511) // 512) * 512]):
                raise ValueError("unexpected_tar_padding")
            name = "/".join(path.parts[1:])
            if name in files:
                raise ValueError("duplicate_archive_entry")
            files[name] = handle.extractfile(member).read()
        trailer = expanded[handle.offset:]
        if len(trailer) < 1024 or len(expanded) % 512 or any(trailer):
            raise ValueError("unexpected_tar_trailer")
    return files


def normalized_manifest_matches(source, packaged):
    """Accept Cargo's structural normalization, not arbitrary packaged settings."""
    original = tomllib.loads(source.decode())
    normalized = tomllib.loads(packaged.decode())
    package = normalized.get("package")
    if not isinstance(original.get("package"), dict) or not isinstance(package, dict):
        return False
    for key in ("build", "autolib", "autobins", "autoexamples", "autotests", "autobenches"):
        if key not in original["package"] and package.get(key) is False:
            package.pop(key)
    for table in ("dependencies", "dev-dependencies", "build-dependencies"):
        if table in original:
            if not isinstance(original[table], dict):
                return False
            original[table] = {name: {"version": value} if isinstance(value, str) else value
                               for name, value in original[table].items()}
    if "test" not in original and "test" in normalized:
        expected = sorted(({"name": Path(name).stem, "path": name}
                           for name in CRATE_FILES if name.startswith("tests/") and name.endswith(".rs")),
                          key=lambda entry: (entry["name"], entry["path"]))
        if normalized["test"] == expected:
            normalized.pop("test")
    return original == normalized


def archive_findings(root, archive, private_prefixes=(), *, candidate_tree=None):
    files = archive_files(archive, private_prefixes)
    expected = CRATE_ARCHIVE_FILES - ({".cargo_vcs_info.json"} if candidate_tree else set())
    findings = inspect_files(files, expected, private_prefixes)
    # A candidate is an explicit immutable Git object, never a free-standing
    # no-VCS archive. The usual path instead requires a clean current commit.
    revision = candidate_tree or "HEAD"
    source = tree_files(root, revision)
    if candidate_tree is None:
        if git(root, "status", "--porcelain=v1", "--untracked-files=all").strip():
            raise ValueError("unclean_archive_origin")
        vcs = json.loads(files[".cargo_vcs_info.json"])
        if (not isinstance(vcs, dict) or set(vcs) != {"git", "path_in_vcs"}
                or not isinstance(vcs.get("git"), dict)):
            raise ValueError("invalid_archive_origin")
        origin = vcs["git"]
        if (set(origin) - {"sha1", "dirty"} or not isinstance(origin.get("sha1"), str)
                or not re.fullmatch(r"[0-9a-f]{40}", origin["sha1"])
                or ("dirty" in origin and origin["dirty"] is not False)
                or vcs["path_in_vcs"] != ""
                or origin["sha1"] != git(root, "rev-parse", "HEAD").decode().strip()):
            raise ValueError("archive_origin_mismatch")
    for name in CRATE_FILES - {"Cargo.toml"}:
        if files.get(name) != source.get(name):
            findings.setdefault(name, Counter())["source_mismatch"] += 1
    if files.get("Cargo.toml.orig") != source.get("Cargo.toml"):
        findings.setdefault("Cargo.toml.orig", Counter())["source_mismatch"] += 1
    if not normalized_manifest_matches(source["Cargo.toml"], files["Cargo.toml"]):
        findings.setdefault("Cargo.toml", Counter())["normalized_manifest_mismatch"] += 1
    return files, findings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--history", action="store_true")
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--candidate-tree", help="full immutable candidate tree ID; allows its no-VCS crate only")
    parser.add_argument("--private-prefix", action="append", default=[])
    args = parser.parse_args()
    findings = Counter()
    inspected = Counter()

    def add(scope, rows):
        for name, rules in rows.items():
            for rule, count in rules.items():
                # Unknown names can themselves contain sensitive material.
                safe_name = name if name in CRATE_ARCHIVE_FILES | REPO_FILES | {"commit-metadata"} else "<unallowed-file>"
                findings[(scope, safe_name, rule)] += count

    files = worktree_files(args.root)
    add("worktree", inspect_files(files, REPO_FILES, args.private_prefix))
    inspected["worktree_files"] = len(files)
    if args.candidate_tree:
        if (not re.fullmatch(r"[0-9a-f]{40}", args.candidate_tree)
                or git(args.root, "cat-file", "-t", args.candidate_tree).strip() != b"tree"):
            raise ValueError("invalid_candidate_tree")
        candidate = tree_files(args.root, args.candidate_tree)
        add("candidate", inspect_files(candidate, REPO_FILES, args.private_prefix))
        for name in set(candidate) | set(files):
            if candidate.get(name) != files.get(name):
                add("candidate", {name: Counter({"worktree_mismatch": 1})})
    if args.history:
        for rows in history_findings(args.root, args.private_prefix):
            add("history", rows)
            inspected["commits"] += 1
    if args.archive:
        archived, rows = archive_findings(args.root, args.archive, args.private_prefix,
                                         candidate_tree=args.candidate_tree)
        add("archive", rows)
        inspected["archive_files"] = len(archived)
    print(json.dumps({"passed": not findings, "checked": inspected,
                      "findings": [{"scope": scope, "file": name, "rule": rule, "count": n}
                                   for (scope, name, rule), n in sorted(findings.items())]}, sort_keys=True))
    return bool(findings)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError, tarfile.TarError, zlib.error) as error:
        known = {"symlink", "nonregular_worktree_entry", "worktree_mode_mismatch", "unsafe_git_entry",
                 "incomplete_or_rewritten_history_view", "unexpected_history_root", "nonlinear_history",
                 "historic_tree_mismatch", "oversized_archive", "unexpected_gzip_metadata",
                 "unexpected_gzip_filename", "unexpected_archive_trailer", "unsafe_archive_entry",
                 "unexpected_tar_metadata", "private_archive_metadata", "unexpected_tar_padding",
                 "duplicate_archive_entry", "unexpected_tar_trailer", "unclean_archive_origin",
                 "invalid_archive_origin", "archive_origin_mismatch", "invalid_candidate_tree"}
        if isinstance(error, subprocess.CalledProcessError): code = "git_query_failed"
        elif isinstance(error, json.JSONDecodeError): code = "invalid_json"
        elif isinstance(error, tomllib.TOMLDecodeError): code = "invalid_manifest"
        elif isinstance(error, tarfile.TarError): code = "invalid_tar"
        elif isinstance(error, zlib.error): code = "invalid_gzip"
        elif isinstance(error, OSError): code = "filesystem_error"
        else: code = str(error) if str(error) in known else "invalid_structure"
        print(json.dumps({"passed": False, "rule": code, "count": 1}))
        raise SystemExit(1)
