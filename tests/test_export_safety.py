#!/usr/bin/env python3
"""Synthetic public-export counterexamples, repository-side; the minimal crate contains no test harness."""
import argparse
import base64
from contextlib import contextmanager
import gzip
import hashlib
import importlib.util
import io
import json
import os
import select
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
import urllib.parse
import zlib

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("check_export", ROOT / "tools/check_export.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def public_fixture(*, legacy=False):
    """Use synthetic bodies, never real execution records or personal data."""
    payload = CHECK.LEGACY_PAYLOAD if legacy else CHECK.CRATE_FILES
    names = CHECK.LEGACY_WORKTREE if legacy else CHECK.REPO_FILES
    files = {name: b"synthetic public fixture\n" for name in names}
    manifest = ["[package]", 'name = "RunDelta"',
                "version = " + json.dumps("0.1.0" if legacy else CHECK.VERSION),
                'license = "MIT"', 'readme = ' + json.dumps('README.md' if legacy else 'docs/CRATE_README.md'),
                'repository = "https://github.com/Dohyeon2006/rundelta"',
                "include = " + json.dumps(["/" + n for n in sorted(payload)])]
    files["Cargo.toml"] = ("\n".join(manifest) + "\n").encode()
    files["LICENSE"] = (ROOT / "LICENSE").read_bytes()
    return files


class SyntheticGit:
    def __init__(self, root):
        self.root = root
        self.paths = set()
        self.env = dict(os.environ, GIT_AUTHOR_NAME="Public Test",
                        GIT_AUTHOR_EMAIL="test@example.invalid",
                        GIT_COMMITTER_NAME="Public Test",
                        GIT_COMMITTER_EMAIL="test@example.invalid")
        self.git("init", "--quiet")
        self.git("config", "gc.auto", "0")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args],
                                       env=self.env, stderr=subprocess.PIPE).decode().strip()

    def stage(self, files):
        for name in self.paths - files.keys():
            (self.root / name).unlink()
        for name, data in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(0o755 if name in CHECK.EXECUTABLE_FILES else 0o644)
        self.paths = set(files)
        self.git("add", "--all")
        return self.git("write-tree")

    def commit(self, files, message="Synthetic public revision"):
        self.stage(files)
        self.git("commit", "--quiet", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def commit_tree(self, tree, parents=(), message="Synthetic DAG node", *, date=None):
        arguments = ["commit-tree", tree]
        for parent in parents:
            arguments.extend(("-p", parent))
        arguments.extend(("-m", message))
        dates = {} if date is None else {"GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date}
        with mock.patch.dict(self.env, dates):
            return self.git(*arguments)

    def object_path(self, oid):
        return self.root / ".git" / "objects" / oid[:2] / oid[2:]

    def raw_object(self, kind, contents):
        """Write malformed graph fixtures without asking Git to repair them."""
        encoded = kind.encode() + b" " + str(len(contents)).encode() + b"\0" + contents
        oid = hashlib.sha1(encoded).hexdigest()
        target = self.object_path(oid)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(zlib.compress(encoded))
        return oid

    def raw_commit(self, tree, parents):
        header = ["tree " + tree, *("parent " + parent for parent in parents),
                  "author Public Test <test@example.invalid> 946684800 +0000",
                  "committer Public Test <test@example.invalid> 946684800 +0000"]
        return self.raw_object("commit", ("\n".join(header) + "\n\nSynthetic malformed graph\n").encode())



@contextmanager
def repository():
    with tempfile.TemporaryDirectory(prefix="rundelta-export-test-") as directory:
        root = Path(directory) / "checkout"
        root.mkdir()
        yield SyntheticGit(root), Path(directory) / "candidate.crate"


def write_archive(path, files, *, vcs=None, extra=None, pax_headers=None, uname="", tar_format=tarfile.PAX_FORMAT):
    packaged = {name: files[name] for name in CHECK.CRATE_FILES}
    packaged["Cargo.toml.orig"] = files["Cargo.toml"]
    if vcs is not None:
        packaged[".cargo_vcs_info.json"] = json.dumps(vcs).encode()
    packaged.update(extra or {})
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", pax_headers=pax_headers, format=tar_format) as archive:
        for name, data in sorted(packaged.items()):
            member = tarfile.TarInfo("RunDelta-" + CHECK.VERSION + "/" + name)
            member.size = len(data)
            member.uname = uname
            archive.addfile(member, io.BytesIO(data))
    path.write_bytes(gzip.compress(buffer.getvalue(), mtime=0))


class ExportSafety(unittest.TestCase):
    def test_paths_and_encoded_variants(self):
        values = ["/" + root + "/example/private-file" for root in
                  ("home", "Users", "root", "tmp", "var/tmp", "private/tmp", "run/user")]
        values.append("C:" + chr(92) + "Users" + chr(92) + "example" + chr(92) + "file")
        for value in values:
            encodings = [value, json.dumps({"p": value}), json.dumps({"p": value}) + "\n",
                         value.encode().hex(), "".join("\\x%02x" % b for b in value.encode()),
                         "".join("\\u%04x" % ord(c) for c in value),
                         "".join("\\%03o" % b for b in value.encode()),
                         urllib.parse.quote(value, safe=""),
                         base64.b64encode(value.encode()).decode(),
                         base64.urlsafe_b64encode(value.encode()).decode().rstrip("=")]
            encodings += [json.dumps(json.dumps(s)) for s in encodings]
            b64 = base64.b64encode(value.encode()).decode()
            encodings.append("\n".join(b64[i:i + 8] for i in range(0, len(b64), 8)))
            encodings.append(base64.b64encode(base64.b64encode(value.encode())).decode())
            encodings.append(base64.b64encode(value.encode().hex().encode()).decode())
            for i, encoded in enumerate(encodings):
                with self.subTest(variant=i):
                    self.assertTrue(CHECK.privacy("README.md", encoded.encode()))

    def test_material_and_secret_encodings(self):
        values = ["docs/" + "external-" + "alpha/record.json",
                  "." + "idea/local.xml", "docs/" + "performance/raw.json",
                  "gh" + "p_" + "a" * 30,
                  "sk" + "-" + "a" * 30, "xox" + "b-" + "1" * 30,
                  "AS" + "IA" + "A" * 16, "gl" + "pat-" + "a" * 30,
                  "-----BEGIN " + "PRIVATE KEY-----"]
        for value in values:
            for encoded in (value, value.encode().hex(), base64.b64encode(value.encode()).decode()):
                self.assertTrue(CHECK.privacy("README.md", encoded.encode()))

    def test_constants_have_exact_file_value_and_version_scope(self):
        for name, constants in CHECK.CONSTANTS.items():
            for value in constants:
                self.assertFalse(CHECK.privacy(name, json.dumps(value).encode()))
                self.assertTrue(CHECK.privacy("README.md", json.dumps(value).encode()))
                self.assertTrue(CHECK.privacy(name, json.dumps(value + "/private").encode()))
        value = CHECK.TEMP_ROOT + "ccAb1234.s"
        self.assertTrue(CHECK.privacy("src/diff/mod.rs", json.dumps(value).encode()))
        self.assertFalse(CHECK.privacy("src/diff/mod.rs", json.dumps(value).encode(), legacy=True))
        self.assertTrue(CHECK.privacy("src/report/mod.rs", json.dumps(CHECK.TEMP_ROOT).encode()))

    def test_nonstandard_private_prefix(self):
        value = "/" + "private-work-root" + "/data"
        for encoded in (value, base64.b64encode(value.encode()).decode(), value.encode().hex()):
            self.assertTrue(CHECK.privacy("README.md", encoded.encode(), [value]))

    def test_current_payload_is_self_contained_in_checkout_and_crate(self):
        # Cargo's normalized manifest and optional VCS metadata are not source
        # files. Reconstruct the explicit source inventory without assuming Git.
        files = {name: (ROOT / name).read_bytes() for name in CHECK.CRATE_FILES}
        if (ROOT / "Cargo.toml.orig").exists():
            files["Cargo.toml"] = (ROOT / "Cargo.toml.orig").read_bytes()
        self.assertFalse(CHECK.inspect_files(files, CHECK.CRATE_FILES))

    def test_inventory_rejects_extra_missing_fixture_and_wrong_license(self):
        files = public_fixture()
        self.assertFalse(CHECK.inspect_files(files, CHECK.REPO_FILES))
        for name in ("target/program", "docs/" + "external-" + "alpha/data.json",
                     "." + "idea/config", "unexpected.txt"):
            self.assertTrue(CHECK.inspect_files(files | {name: b"synthetic"}, CHECK.REPO_FILES))
        for fixture in ("tests/public_cli.py", "tests/fixtures/runtime_boundary_probe.c"):
            missing = {k: v for k, v in files.items() if k != fixture}
            self.assertTrue(CHECK.inspect_files(missing, CHECK.REPO_FILES))
        self.assertTrue(CHECK.inspect_files(files | {"LICENSE": b"incorrect"}, CHECK.REPO_FILES))

    def test_wrong_cargo_include_and_duplicate_entry_fail(self):
        files = public_fixture()
        manifest = files["Cargo.toml"]
        for changed in (manifest.replace(b'"/src/cli.rs", ', b""),
                        manifest.replace(b'include = [', b'include = ["/README.md", ')):
            self.assertNotEqual(changed, manifest)
            self.assertTrue(CHECK.inspect_files(files | {"Cargo.toml": changed}, CHECK.REPO_FILES))

    def test_repo_and_crate_inventories_are_distinct_and_exact(self):
        self.assertEqual(len(CHECK.CRATE_FILES), 20)
        self.assertEqual(CHECK.CRATE_ARCHIVE_FILES - CHECK.CRATE_FILES,
                         {"Cargo.toml.orig", ".cargo_vcs_info.json"})
        self.assertTrue(CHECK.CRATE_FILES < CHECK.REPO_FILES)
        self.assertNotIn("AGENTS.md", CHECK.REPO_FILES)
        self.assertNotIn("WORKSPACE.md", CHECK.REPO_FILES)
        self.assertNotIn("docs/README_MERGE_PROPOSAL.md", CHECK.REPO_FILES)
        files = public_fixture()
        manifest = files["Cargo.toml"]
        for added in ("/tests/public_cli.py", "/docs/FIELD_CASES.md"):
            changed = manifest.replace(b"include = [", ("include = [" + json.dumps(added) + ", ").encode())
            self.assertTrue(CHECK.inspect_files(files | {"Cargo.toml": changed}, CHECK.REPO_FILES))
        with repository() as (repo, archive):
            tree = repo.stage(files)
            write_archive(archive, files)
            packaged = CHECK.archive_files(archive)
            self.assertFalse(CHECK.inspect_files(packaged, CHECK.CRATE_ARCHIVE_FILES - {".cargo_vcs_info.json"}))
            for missing in CHECK.CRATE_FILES:
                self.assertTrue(CHECK.inspect_files({name: data for name, data in packaged.items()
                                                    if name != missing}, CHECK.CRATE_ARCHIVE_FILES - {".cargo_vcs_info.json"}))

    def test_git_and_worktree_modes_are_exact(self):
        with repository() as (repo, _):
            files = public_fixture()
            tree = repo.stage(files)
            CHECK.tree_files(repo.root, tree)
            CHECK.worktree_files(repo.root)
            for name, mode in [("README.md", "100755"),
                               *((name, "100644") for name in CHECK.EXECUTABLE_FILES)]:
                with self.subTest(name=name, mode=mode):
                    repo.stage(files)
                    repo.git("update-index", "--chmod=" + ("+x" if mode == "100755" else "-x"), name)
                    changed = repo.git("write-tree")
                    with self.assertRaisesRegex(ValueError, "unsafe_git_entry"):
                        CHECK.tree_files(repo.root, changed)
            repo.stage(files)
            for name in ("README.md", *CHECK.EXECUTABLE_FILES):
                path = repo.root / name
                original_mode = path.stat().st_mode
                path.chmod(0o644 if name in CHECK.EXECUTABLE_FILES else 0o755)
                with self.assertRaisesRegex(ValueError, "worktree_mode_mismatch"):
                    CHECK.worktree_files(repo.root)
                path.chmod(original_mode)
            name = next(iter(CHECK.EXECUTABLE_FILES))
            oid = repo.git("hash-object", "-w", name)
            repo.git("update-index", "--cacheinfo", "120000," + oid + "," + name)
            with self.assertRaisesRegex(ValueError, "unsafe_git_entry"):
                CHECK.tree_files(repo.root, repo.git("write-tree"))
            repo.stage(files)
            path = repo.root / name
            path.unlink()
            path.symlink_to("README.md")
            with self.assertRaisesRegex(ValueError, "symlink"):
                CHECK.worktree_files(repo.root)

    def test_allowed_executable_scripts_still_reject_secrets_and_encodings(self):
        secret = "gh" + "p_" + "a" * 30
        for name in CHECK.EXECUTABLE_FILES:
            for value in (secret.encode(), secret.encode().hex().encode(),
                          base64.b64encode(secret.encode())):
                self.assertTrue(CHECK.privacy(name, value, public=True)["credential_shape"])

    def test_published_document_references_are_exact_not_privacy_exceptions(self):
        known = b"See docs/FIELD_CASES.md"
        self.assertFalse(CHECK.privacy("README.md", known, public=True))
        for value, rule in (
            (b"docs/FIELD_CASES-private.md", "excluded_material"),
            (b"docs/external-alpha/unknown-record.json", "excluded_material"),
            (b"unapproved/FIELD_CASES.md", "excluded_material"),
            (b"unapproved/docs/FIELD_CASES.md", "excluded_material"),
            (("/" + "home/example/private").encode(), "private_path"),
        ):
            for encoded in (value, value.hex().encode(), base64.b64encode(value)):
                with self.subTest(value=value, rule=rule):
                    self.assertGreater(CHECK.privacy("README.md", encoded, public=True)[rule], 0)
                    self.assertGreater(CHECK.privacy("README.md", known + b"\n" + encoded, public=True)[rule], 0)

    def test_synthetic_context_permissions_do_not_allow_standalone_operands(self):
        for name, contexts in CHECK.SYNTHETIC_MATERIAL_CONTEXTS.items():
            for context, operand in contexts:
                for value in (operand.encode(), operand.encode().hex().encode(),
                              base64.b64encode(operand.encode())):
                    self.assertGreater(CHECK.privacy(name, value, public=True)["excluded_material"], 0)
                original = (ROOT / name).read_bytes()
                self.assertEqual(CHECK.privacy(name, original, public=True)["excluded_material"], 0)
                changed = original.decode().replace(operand, operand + "-unreviewed")
                self.assertGreater(CHECK.privacy(name, changed.encode(), public=True)["excluded_material"], 0)

    def test_frozen_relative_fixture_path_requires_its_exact_source_context(self):
        for name, contexts in CHECK.SYNTHETIC_PATH_CONTEXTS.items():
            for context, operand in contexts:
                original = (ROOT / name).read_bytes()
                self.assertEqual(CHECK.privacy(name, original, public=True)["private_path"], 0)
                for value in (operand.encode(), operand.encode().hex().encode(),
                              base64.b64encode(operand.encode())):
                    self.assertGreater(CHECK.privacy(name, value, public=True)["private_path"], 0)
                changed = original.decode().replace(operand, operand + "-unreviewed")
                self.assertGreater(CHECK.privacy(name, changed.encode(), public=True)["private_path"], 0)

    def test_synthetic_contexts_keep_credentials_and_private_prefix_checks(self):
        credential = ("gh" + "p_" + "a" * 30).encode()
        for name, contexts in CHECK.SYNTHETIC_MATERIAL_CONTEXTS.items():
            for context, operand in contexts:
                for encoded in (credential, credential.hex().encode(), base64.b64encode(credential)):
                    self.assertGreater(CHECK.privacy(name, context.encode() + b"\n" + encoded,
                                                     public=True)["credential_shape"], 0)
                self.assertGreater(CHECK.privacy(name, context.encode(), [operand],
                                                 public=True)["explicit_private_prefix"], 0)

    def test_repeated_short_base64_tokens_have_one_decode_and_bounded_queue(self):
        for token in (b"YQ", b"YR"):
            for repetitions in (8_000, 40_000):
                with self.subTest(token=token, repetitions=repetitions):
                    with mock.patch.object(CHECK, "_decode_base64_token", wraps=CHECK._decode_base64_token) as decode, \
                         mock.patch.object(CHECK, "reference_context", wraps=CHECK.reference_context) as context, \
                         mock.patch.object(CHECK, "valid_reference", wraps=CHECK.valid_reference) as reference:
                        findings = CHECK.privacy("README.md", (token + b" ") * repetitions, public=True)
                    self.assertFalse(findings)
                    self.assertEqual(decode.call_count, 1)
                    self.assertEqual(context.call_count, 1)
                    self.assertEqual(reference.call_count, 1)

    def test_distinct_short_base64_attempt_budget_has_an_exact_boundary(self):
        values = b"YQ Yg Yw ZA"
        for budget, rejected in ((4, False), (3, True)):
            with self.subTest(attempt_budget=budget):
                with mock.patch.object(CHECK, "BASE64_MAX_ATTEMPTS", budget), \
                     mock.patch.object(CHECK, "_decode_base64_token", wraps=CHECK._decode_base64_token) as decode:
                    findings = CHECK.privacy("README.md", values, material=False)
                self.assertEqual(findings["base64_attempt_budget_exceeded"], int(rejected))
                self.assertEqual(decode.call_count, budget)
                self.assertEqual(bool(findings), rejected)

    def test_base64_input_and_match_budgets_count_cache_hits_and_fail_closed(self):
        # Two original rule views; one-character decoded outputs contain no
        # further Base64 matches. This fixes operation counts without timing.
        for constant, boundary, rule in (
            ("BASE64_MAX_INPUT_CHARS", 8, "base64_input_budget_exceeded"),
            ("BASE64_MAX_MATCHES", 4, "base64_match_budget_exceeded"),
        ):
            for budget, rejected in ((boundary, False), (boundary - 1, True)):
                with self.subTest(limit=constant, budget=budget):
                    with mock.patch.object(CHECK, constant, budget), \
                         mock.patch.object(CHECK, "_decode_base64_token", wraps=CHECK._decode_base64_token) as decode:
                        findings = CHECK.privacy("README.md", b"YQ YQ", material=False)
                    self.assertEqual(findings[rule], int(rejected))
                    self.assertEqual(decode.call_count, 1)
                    self.assertEqual(bool(findings), rejected)

    def test_sensitive_short_base64_mixed_with_repetition_is_never_silently_skipped(self):
        private = ("/" + "home/x").encode()
        tokens = [b"YQ"] * 40_000
        for suffix in (b"", b"\0", b"\x01"):
            canary = base64.b64encode(private + suffix)
            for position in (0, 20_000, 40_000):
                with self.subTest(position=position, control=suffix):
                    mixed = b" ".join(tokens[:position] + [canary] + tokens[position:])
                    with mock.patch.object(CHECK, "_decode_base64_token", wraps=CHECK._decode_base64_token) as decode, \
                         mock.patch.object(CHECK, "reference_context", wraps=CHECK.reference_context) as context, \
                         mock.patch.object(CHECK, "valid_reference", wraps=CHECK.valid_reference) as reference:
                        findings = CHECK.privacy("README.md", mixed, public=True)
                    self.assertGreater(findings["private_path"], 0)
                    self.assertLessEqual(decode.call_count, 3)
                    self.assertEqual(context.call_count, 1)
                    self.assertEqual(reference.call_count, 2)
        with mock.patch.object(CHECK, "BASE64_MAX_ATTEMPTS", 1):
            findings = CHECK.privacy("README.md", b"YQ " + canary, public=True)
        self.assertEqual(findings["base64_attempt_budget_exceeded"], 1)
        self.assertTrue(findings)

    def test_utf8_base64_controls_preserve_sensitive_substrings_and_nested_detection(self):
        private = ("/" + "home/canary-private/secret").encode()
        credential = ("gh" + "p_" + "A" * 30).encode()
        for label, payload, rule in (
            ("path-nul", private + b"\0", "private_path"),
            ("credential-nul", credential + b"\0", "credential_shape"),
            ("path-control", private + b"\x01", "private_path"),
        ):
            direct = base64.b64encode(payload)
            self.assertEqual(CHECK._decode_base64_token(direct.decode()), payload.decode())
            for encoding, encoded in (
                ("base64", direct),
                ("nested-base64", base64.b64encode(direct)),
                ("base64-of-hex", base64.b64encode(payload.hex().encode())),
                ("urlsafe-unpadded", base64.urlsafe_b64encode(payload).rstrip(b"=")),
            ):
                with self.subTest(canary=label, encoding=encoding):
                    prefixes = [private.decode()] if rule == "private_path" else []
                    findings = CHECK.privacy("README.md", encoded, prefixes, public=True)
                    self.assertGreater(findings[rule], 0)
                    self.assertEqual(findings["base64_attempt_budget_exceeded"], 0)
                    if prefixes:
                        self.assertGreater(findings["explicit_private_prefix"], 0)

    def test_short_base64_paths_never_acquire_source_context(self):
        for private in (("/" + "home/x").encode(), ("/" + "tmp/?").encode()):
            for label, encoded in (
                ("standard-padded", base64.b64encode(private)),
                ("standard-unpadded", base64.b64encode(private).rstrip(b"=")),
                ("urlsafe-padded", base64.urlsafe_b64encode(private)),
                ("urlsafe-unpadded", base64.urlsafe_b64encode(private).rstrip(b"=")),
            ):
                with self.subTest(encoding=label, size=len(private)):
                    self.assertGreater(CHECK.privacy("README.md", encoded, public=True)["private_path"], 0)
        operand = next(iter(CHECK.SYNTHETIC_MATERIAL_CONTEXTS[".gitignore"]))[1].encode()
        self.assertGreater(CHECK.privacy(".gitignore", base64.b64encode(operand), public=True)["excluded_material"], 0)

    def test_cargo_anchor_permission_is_only_for_exact_package_include(self):
        for legacy in (False, True):
            files = public_fixture(legacy=legacy)
            inventory = CHECK.LEGACY_WORKTREE if legacy else CHECK.REPO_FILES
            payload = CHECK.LEGACY_PAYLOAD if legacy else CHECK.CRATE_FILES
            readme = "README.md" if legacy else "docs/CRATE_README.md"
            self.assertFalse(CHECK.inspect_files(files, inventory, legacy=legacy,
                                                payload=payload, readme=readme))
            manifest = files["Cargo.toml"]
            changed = manifest + b'\n[package.metadata]\ncomment = "/' + b'docs/' + b'FIELD_' + b'CASES.md"\n'
            self.assertGreater(CHECK.inspect_files(files | {"Cargo.toml": changed}, inventory,
                                                   legacy=legacy, payload=payload, readme=readme)
                               ["Cargo.toml"]["excluded_material"], 0)
        value = b"/" + b"docs/" + b"FIELD_" + b"CASES.md"
        self.assertGreater(CHECK.privacy("README.md", value, public=True)["excluded_material"], 0)

    def test_policy_ast_roles_do_not_transfer_to_unparsed_or_encoded_values(self):
        source = (ROOT / "tools/check_export.py").read_text()
        tree = CHECK.ast.parse(source)
        assignment = next(node for node in tree.body if isinstance(node, CHECK.ast.Assign) and
                          any(isinstance(target, CHECK.ast.Name) and target.id == "SOURCE_REFERENCES"
                              for target in node.targets))
        declaration = CHECK.ast.get_source_segment(source, assignment)
        self.assertEqual(CHECK.privacy("tools/check_export.py", declaration.encode(), public=True)["excluded_material"], 0)
        self.assertGreater(CHECK.privacy("tools/check_export.py", base64.b64encode(declaration.encode()),
                                         public=True)["excluded_material"], 0)
        unknown = "unapproved/" + "FIELD_" + "CASES.md"
        invalid = "SOURCE_REFERENCES = " + repr({"README.md": {unknown: "docs/" + "FIELD_" + "CASES.md"}})
        self.assertGreater(CHECK.privacy("tools/check_export.py", invalid.encode(), public=True)["excluded_material"], 0)

    def test_five_scoped_path_and_regex_locations_have_zero_material_findings(self):
        for name, role in (
            ("examples/holdout_cases.py", "frozen-input"),
            ("tests/check_regression_freeze.py", "manifest-load"),
            ("tests/full_gate.py", "clean-package-checker"),
            ("tests/full_gate.py", "optimization-checker"),
            ("tools/check_export.py", "ignore-directive-parser"),
        ):
            with self.subTest(source=name, role=role):
                self.assertEqual(CHECK.privacy(name, (ROOT / name).read_bytes(),
                                               public=True)["excluded_material"], 0)

    def test_scoped_path_roles_reject_unknown_roots_addition_suffix_and_copy(self):
        for name, target_paths in CHECK.SCOPED_PYTHON_TARGETS.items():
            original = (ROOT / name).read_text()
            tree = CHECK.ast.parse(original)
            divisions = [node for node in CHECK.ast.walk(tree) if isinstance(node, CHECK.ast.BinOp)
                         and isinstance(node.op, CHECK.ast.Div) and isinstance(node.right, CHECK.ast.Constant)
                         and node.right.value in target_paths]
            for node in divisions:
                spelling = CHECK.ast.get_source_segment(original, node)
                token = CHECK.ast.get_source_segment(original, node.right)
                for label, replacement in (
                    ("unknown-root", "UNKNOWN_ROOT / " + token),
                    ("addition", CHECK.ast.get_source_segment(original, node.left) + " + " + token),
                    ("suffix", spelling.replace(token, repr(node.right.value + "-unreviewed"))),
                    ("traversal", spelling.replace(token, repr("../" + node.right.value))),
                ):
                    with self.subTest(source=name, mutation=label):
                        changed = original.replace(spelling, replacement, 1)
                        self.assertGreater(CHECK.privacy(name, changed.encode(), public=True)["excluded_material"], 0)
                for copied in (node.right.value.encode(), repr(node.right.value).encode(),
                               base64.b64encode(spelling.encode()), spelling.encode().hex().encode()):
                    self.assertGreater(CHECK.privacy(name, copied, public=True)["excluded_material"], 0)

    def test_repository_root_and_clean_source_origins_are_required(self):
        name = "examples/holdout_cases.py"
        original = (ROOT / name).read_text()
        changed = original.replace("ROOT = Path(__file__).resolve().parents[1]", "ROOT = Path('unreviewed-root')", 1)
        self.assertGreater(CHECK.privacy(name, changed.encode(), public=True)["excluded_material"], 0)
        name = "tests/full_gate.py"
        original = (ROOT / name).read_text()
        for before, after in (("source = work / \"source\"", "source = UNKNOWN_ROOT"),
                              ("extract_clean_tree(exported, source)", "unreviewed_extract(exported, source)")):
            changed = original.replace(before, after, 1)
            self.assertNotEqual(changed, original)
            self.assertGreater(CHECK.privacy(name, changed.encode(), public=True)["excluded_material"], 0)

    def test_ignore_regex_permission_requires_the_exact_ast_branch_and_call(self):
        name = "tools/check_export.py"
        original = (ROOT / name).read_text()
        tree = CHECK.ast.parse(original)
        pattern = next(node for node in CHECK.ast.walk(tree)
                       if isinstance(node, CHECK.ast.Constant) and isinstance(node.value, str)
                       and CHECK.hashlib.sha256(node.value.encode()).hexdigest() == CHECK.GITIGNORE_PATTERN_SHA256)
        spelling = CHECK.ast.get_source_segment(original, pattern)
        parents = {child: node for node in CHECK.ast.walk(tree) for child in CHECK.ast.iter_child_nodes(node)}
        call = CHECK.ast.get_source_segment(original, parents[pattern])
        for label, changed in (
            ("changed-value", original.replace(spelling, repr(pattern.value + "../"), 1)),
            ("string-addition", original.replace(spelling, "(" + spelling + " + '')", 1)),
            ("adjacent-string", original.replace(spelling, spelling + " ''", 1)),
            ("extra-keyword", original.replace(call, call[:-1] + ", flags=0)", 1)),
            ("extra-positional", original.replace(call, call[:-1] + ", 0)", 1)),
            ("outside-branch", original.replace('if name == \".gitignore\":', 'if name == \"not-the-ignore-parser\":', 1)),
            ("copied-literal", original + "\nCOPIED_REGEX = " + spelling + "\n"),
        ):
            with self.subTest(mutation=label):
                self.assertNotEqual(changed, original)
                self.assertGreater(CHECK.privacy(name, changed.encode(), public=True)["excluded_material"], 0)
        for encoded in (base64.b64encode(pattern.value.encode()), pattern.value.encode().hex().encode()):
            self.assertGreater(CHECK.privacy(name, encoded, public=True)["excluded_material"], 0)

    def test_scoped_roles_never_exempt_credentials_paths_or_private_prefixes(self):
        credential = ("gh" + "p_" + "a" * 30).encode()
        private = ("/" + "home/example/new-private").encode()
        for name in (*CHECK.SCOPED_PYTHON_TARGETS, "tools/check_export.py"):
            original = (ROOT / name).read_bytes()
            self.assertGreater(CHECK.privacy(name, original, ["docs/"], public=True)["explicit_private_prefix"], 0)
            for encoded in (credential, base64.b64encode(credential)):
                self.assertGreater(CHECK.privacy(name, original + b"\n" + encoded,
                                                 public=True)["credential_shape"], 0)
            for encoded in (private, base64.b64encode(private)):
                self.assertGreater(CHECK.privacy(name, original + b"\n" + encoded,
                                                 public=True)["private_path"], 0)

    def test_actual_synthetic_fixture_and_policy_sources_have_zero_material_findings(self):
        for name in ("tests/test_release_closure.py", "tests/test_export_safety.py", "tools/check_export.py"):
            with self.subTest(source=name):
                original = (ROOT / name).read_bytes()
                self.assertEqual(CHECK.privacy(name, original, public=True)["excluded_material"], 0)
        name = "tests/test_release_closure.py"
        original = (ROOT / name).read_text()
        context, operand = CHECK.SYNTHETIC_MATERIAL_CONTEXTS[name][0]
        indent = context[:len(context) - len(context.lstrip())]
        for label, replacement in (
            ("prepended-literal", indent + 'b"unapproved/" ' + context.lstrip()),
            ("appended-literal", context + ' b"-unreviewed"'),
            ("changed-literal", context.replace(operand, operand + "-unreviewed")),
        ):
            with self.subTest(context=label):
                altered = original.replace(context, replacement, 1)
                self.assertGreater(CHECK.privacy(name, altered.encode(), public=True)["excluded_material"], 0)
        for encoded in (context.encode().hex().encode(), base64.b64encode(context.encode())):
            self.assertGreater(CHECK.privacy(name, encoded, public=True)["excluded_material"], 0)

    def test_cargo_gnu_zero_ids_and_nul_checksum_layout_is_supported(self):
        with repository() as (repo, archive):
            files = public_fixture()
            commit = repo.commit(files)
            write_archive(archive, files, vcs={"git": {"sha1": commit}, "path_in_vcs": ""},
                          tar_format=tarfile.GNU_FORMAT)
            expanded = bytearray(gzip.decompress(archive.read_bytes()))
            with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as handle:
                offsets = [member.offset for member in handle]
            self.assertEqual(len(offsets), 22)
            def checksum(buffer, offset):
                buffer[offset + 148:offset + 156] = b" " * 8
                total = sum(buffer[offset:offset + 512])
                buffer[offset + 148:offset + 156] = ("%07o" % total).encode() + b"\0"
            for offset in offsets:
                expanded[offset + 108:offset + 124] = b"\0" * 16
                checksum(expanded, offset)
            valid = bytes(expanded)
            archive.write_bytes(gzip.compress(valid, mtime=0))
            self.assertEqual(set(CHECK.archive_files(archive)), CHECK.CRATE_ARCHIVE_FILES)
            self.assertFalse(CHECK.archive_findings(repo.root, archive)[1])
            for label, position in (("uid-hidden-byte", 115), ("gid-hidden-byte", 123),
                                    ("mtime-not-authorized-empty", 136), ("wrong-checksum", 148)):
                with self.subTest(field=label):
                    changed = bytearray(valid)
                    if label == "mtime-not-authorized-empty":
                        changed[136:148] = b"\0" * 12
                    elif label != "wrong-checksum":
                        changed[position] = ord("1")
                    checksum(changed, 0)
                    if label == "wrong-checksum":
                        changed[148] = ord("7")
                    archive.write_bytes(gzip.compress(changed, mtime=0))
                    with self.assertRaises((ValueError, tarfile.TarError)):
                        CHECK.archive_files(archive)

    def test_gnu_production_and_ustar_headers_have_exact_crate_inventory(self):
        with repository() as (repo, archive):
            files = public_fixture()
            commit = repo.commit(files)
            vcs = {"git": {"sha1": commit}, "path_in_vcs": ""}
            for label, profile in (("GNU", tarfile.GNU_FORMAT), ("USTAR", tarfile.USTAR_FORMAT)):
                with self.subTest(profile=label):
                    write_archive(archive, files, vcs=vcs, tar_format=profile)
                    self.assertEqual(set(CHECK.archive_files(archive)), CHECK.CRATE_ARCHIVE_FILES)
                    self.assertFalse(CHECK.archive_findings(repo.root, archive)[1])

    def test_tar_header_every_field_and_nul_padding_are_validated(self):
        with repository() as (repo, archive):
            files = public_fixture()
            commit = repo.commit(files)
            write_archive(archive, files, vcs={"git": {"sha1": commit}, "path_in_vcs": ""},
                          tar_format=tarfile.GNU_FORMAT)
            original = gzip.decompress(archive.read_bytes())
            for label in ("name-padding", "numeric-terminator", "owner", "reserved", "magic", "unknown-name", "checksum"):
                with self.subTest(field=label):
                    expanded = bytearray(original)
                    if label == "name-padding": expanded[expanded.index(0, 0, 100) + 1] = ord("X")
                    if label == "numeric-terminator": expanded[107] = ord(" ")
                    if label == "owner": expanded[265] = ord("X")
                    if label == "reserved": expanded[500] = ord("X")
                    if label == "magic": expanded[257] = ord("X")
                    if label == "unknown-name":
                        name = ("RunDelta-" + CHECK.VERSION + "/unknown").encode()
                        expanded[:100] = name.ljust(100, b"\0")
                    expanded[148:156] = b" " * 8
                    checksum = sum(expanded[:512])
                    expanded[148:156] = ("%06o" % checksum).encode() + b"\0 "
                    if label == "checksum": expanded[148] = ord("7")
                    archive.write_bytes(gzip.compress(expanded, mtime=0))
                    with self.assertRaises((ValueError, tarfile.TarError)):
                        CHECK.archive_files(archive)
            archive.write_bytes(gzip.compress(original, mtime=0))
            with self.assertRaisesRegex(ValueError, "private_archive_metadata"):
                CHECK.archive_files(archive, ["RunDelta-"])

    def test_reviewed_path_content_is_revoked_by_any_change(self):
        for name, expected in CHECK.REVIEWED_PATH_CONTENT.items():
            original = (ROOT / name).read_bytes()
            self.assertEqual(CHECK.hashlib.sha256(original).hexdigest(), expected)
            secret_path = ("/" + "home/example/new-private-path").encode()
            for appended in (secret_path, base64.b64encode(secret_path)):
                self.assertTrue(CHECK.privacy(name, original + appended, public=True)["private_path"])
            # An exact reviewed path body never exempts a credential or a
            # caller-provided private prefix, even before bytes change.
            prefix = original.decode("utf-8")[:20]
            if prefix:
                self.assertTrue(CHECK.privacy(name, original, [prefix], public=True)["explicit_private_prefix"])

    def test_gitlink_is_rejected_even_at_a_reviewed_script_path(self):
        with repository() as (repo, _):
            files = public_fixture()
            commit = repo.commit(files)
            name = next(iter(CHECK.EXECUTABLE_FILES))
            repo.git("update-index", "--cacheinfo", "160000," + commit + "," + name)
            with self.assertRaisesRegex(ValueError, "unsafe_git_entry"):
                CHECK.tree_files(repo.root, repo.git("write-tree"))

    def test_tar_executable_or_special_permission_bits_are_rejected(self):
        with repository() as (_, archive):
            for permissions in (0o755, 0o4644, 0o2644):
                buffer = io.BytesIO()
                with tarfile.open(fileobj=buffer, mode="w") as handle:
                    member = tarfile.TarInfo("RunDelta-" + CHECK.VERSION + "/LICENSE")
                    member.mode = permissions
                    handle.addfile(member, io.BytesIO())
                archive.write_bytes(gzip.compress(buffer.getvalue(), mtime=0))
                with self.assertRaisesRegex(ValueError, "unexpected_tar_metadata"):
                    CHECK.archive_files(archive)

    def test_workflow_is_required_source_only_and_not_a_privacy_exception(self):
        name = ".github/workflows/ci.yml"
        files = public_fixture()
        self.assertIn(name, CHECK.REPO_FILES)
        self.assertNotIn(name, CHECK.CRATE_FILES)
        self.assertTrue(CHECK.inspect_files({k: v for k, v in files.items() if k != name},
                                            CHECK.REPO_FILES))
        private = "/" + "home" + "/example/synthetic-private"
        for data in (private.encode(), base64.b64encode(private.encode())):
            self.assertTrue(CHECK.inspect_files(files | {name: data}, CHECK.REPO_FILES))
        manifest = files["Cargo.toml"].replace(
            b'include = [', b'include = ["/.github/workflows/ci.yml", ')
        self.assertTrue(CHECK.inspect_files(files | {"Cargo.toml": manifest}, CHECK.REPO_FILES))
        with repository() as (repo, archive):
            tree = repo.stage(files)
            write_archive(archive, files, extra={name: b"synthetic workflow"})
            with self.assertRaisesRegex(ValueError, "unsafe_archive_entry"):
                CHECK.archive_findings(repo.root, archive, candidate_tree=tree)

    def test_worktree_scans_ignored_and_untracked_files(self):
        with repository() as (repo, _):
            files = public_fixture()
            files[".gitignore"] = b"ignored/\n"
            repo.commit(files)
            self.assertFalse(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.REPO_FILES))
            (repo.root / "ignored").mkdir()
            (repo.root / "ignored" / "secret").write_bytes(b"synthetic")
            self.assertTrue(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.REPO_FILES))

    def test_worktree_rejects_empty_output_directory(self):
        with repository() as (repo, _):
            repo.commit(public_fixture())
            (repo.root / "target").mkdir()
            self.assertTrue(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.REPO_FILES))

    def synthetic_git_query(self, script, *, max_bytes=32):
        """Exercise the query reader with an owned child, never a real Git call."""
        actual = subprocess.Popen
        children = []
        def launch(command, **kwargs):
            self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
            self.assertTrue(kwargs["start_new_session"])
            child = actual([sys.executable, "-B", "-c", script], **kwargs)
            children.append(child)
            return child
        try:
            with mock.patch.object(CHECK, "GIT_QUERY_TIMEOUT_SECONDS", 0.5), \
                 mock.patch.object(CHECK, "GIT_REAP_TIMEOUT_SECONDS", 2.0), \
                 mock.patch.object(CHECK, "GIT_MAX_QUERY_BYTES", 32), \
                 mock.patch.object(CHECK.subprocess, "Popen", side_effect=launch):
                return CHECK.git(ROOT, "synthetic-query-fixture", max_bytes=max_bytes)
        finally:
            # Keep emergency cleanup separate from the assertion: it must not
            # make a production failure to reap look like a passing test.
            reaped = bool(children) and all(child.returncode is not None for child in children)
            for child in children:
                if child.returncode is None:
                    child.kill()
                    child.wait(timeout=2)
            self.assertTrue(reaped, "query returned/raised without reaping its owned child")

    def test_git_query_deadline_reaps_no_output_and_small_output_stalls(self):
        for label, script, limit in (
            ("no-output-bounded", "import time; time.sleep(30)", 32),
            ("no-output-default", "import time; time.sleep(30)", None),
            ("small-output", "import os,time; os.write(1,b'x'); time.sleep(30)", 32),
            ("stdout-eof-before-exit", "import os,time; os.close(1); time.sleep(30)", None),
        ):
            with self.subTest(stall=label):
                with self.assertRaisesRegex(ValueError, "git_query_timeout"):
                    self.synthetic_git_query(script, max_bytes=limit)

    def test_git_query_keeps_incremental_output_limit_and_reaps_overflow(self):
        for limit, rule in ((32, "history_work_budget_exceeded"), (None, "git_output_budget_exceeded")):
            with self.subTest(limit=limit):
                with self.assertRaisesRegex(ValueError, rule):
                    self.synthetic_git_query("import os,time; os.write(1,b'x'*4096); time.sleep(30)",
                                             max_bytes=limit)

    def test_git_query_success_failure_and_stderr_flood_are_bounded(self):
        script = "import os; os.write(2,b'e'*262144); os.write(1,b'ok')"
        for limit in (2, None):
            with self.subTest(limit=limit):
                self.assertEqual(self.synthetic_git_query(script, max_bytes=limit), b"ok")
        failed = "import os; os.write(2,b'e'*262144); os.write(1,b'partial'); raise SystemExit(7)"
        with self.assertRaises(subprocess.CalledProcessError) as failure:
            self.synthetic_git_query(failed)
        self.assertEqual(failure.exception.returncode, 7)

    def test_git_failure_cleans_descendants_after_the_leader_is_reaped(self):
        actual = subprocess.Popen
        descriptors = set()
        leader = unrelated = None
        descendant_fd = None
        def pipe():
            pair = os.pipe()
            descriptors.update(pair)
            return pair
        def close(descriptor):
            if descriptor in descriptors:
                descriptors.remove(descriptor)
                os.close(descriptor)
        ready_read, ready_write = pipe()
        release_read, release_write = pipe()
        script = (
            "import os,sys,time\n"
            "ready,release=map(int,sys.argv[1:])\n"
            "child=os.fork()\n"
            "if child == 0:\n"
            " os.close(ready); os.close(release); time.sleep(30); os._exit(0)\n"
            "os.write(ready,(str(child)+'\\n').encode()); os.close(ready)\n"
            "os.read(release,1); os.close(release); os._exit(0)\n"
        )
        def launch(command, **kwargs):
            nonlocal leader, descendant_fd
            self.assertTrue(kwargs["start_new_session"])
            kwargs["pass_fds"] = (ready_write, release_read)
            leader = actual([sys.executable, "-B", "-c", script, str(ready_write), str(release_read)], **kwargs)
            close(ready_write)
            close(release_read)
            self.assertTrue(select.select([ready_read], [], [], 2)[0], "descendant did not become ready")
            marker = os.read(ready_read, 64)
            self.assertTrue(marker.endswith(b"\n") and marker[:-1].isdigit(), "invalid descendant marker")
            descendant_fd = os.pidfd_open(int(marker))
            descriptors.add(descendant_fd)
            self.assertFalse(select.select([descendant_fd], [], [], 0)[0])
            os.write(release_write, b"x")
            close(release_write)
            leader.wait(timeout=2)
            self.assertEqual(leader.returncode, 0)
            # Return an already reaped leader while its child retains stdout;
            # returncode-only cleanup must not leave this dedicated group live.
            return leader
        try:
            unrelated = actual([sys.executable, "-B", "-c", "import time; time.sleep(30)"],
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, start_new_session=True)
            with mock.patch.object(CHECK, "GIT_QUERY_TIMEOUT_SECONDS", 0.25), \
                 mock.patch.object(CHECK.subprocess, "Popen", side_effect=launch), \
                 mock.patch.object(CHECK.os, "killpg", wraps=CHECK.os.killpg) as killed:
                with self.assertRaisesRegex(ValueError, "git_query_timeout"):
                    CHECK.git(ROOT, "synthetic-orphan-query", max_bytes=32)
                killed.assert_called_once_with(leader.pid, CHECK.signal.SIGKILL)
            self.assertIsNotNone(descendant_fd)
            self.assertTrue(select.select([descendant_fd], [], [], 2)[0],
                            "failed query left its stdout-holding descendant alive")
            self.assertIsNone(unrelated.poll(), "an unrelated process group was terminated")
        finally:
            # Cleanup must also work when a regression assertion or the query
            # itself fails. pidfd targets the captured descendant identity.
            try:
                if descendant_fd is not None:
                    try:
                        CHECK.signal.pidfd_send_signal(descendant_fd, CHECK.signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    select.select([descendant_fd], [], [], 2)
                if leader is not None:
                    if leader.returncode is None:
                        try:
                            os.killpg(leader.pid, CHECK.signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                    leader.wait(timeout=2)
                    leader.stdout.close()
                if unrelated is not None:
                    if unrelated.poll() is None:
                        unrelated.kill()
                    unrelated.wait(timeout=2)
            finally:
                for descriptor in list(descriptors):
                    close(descriptor)

    def test_git_success_does_not_expand_group_cleanup_and_caller_group_is_guarded(self):
        with mock.patch.object(CHECK, "stop_git_query", wraps=CHECK.stop_git_query) as cleanup:
            self.assertEqual(self.synthetic_git_query("import os; os.write(1,b'ok')"), b"ok")
        cleanup.assert_not_called()
        for group in (os.getpid(), os.getpgrp()):
            with self.subTest(group=group), mock.patch.object(CHECK.os, "killpg") as kill:
                with self.assertRaisesRegex(ValueError, "git_termination_failed"):
                    CHECK.stop_git_query(mock.Mock(pid=group))
                kill.assert_not_called()

    def test_git_cleanup_never_uses_an_unbounded_wait(self):
        process = mock.Mock(pid=os.getpgrp() + os.getpid() + 1)
        process.wait.side_effect = subprocess.TimeoutExpired("synthetic-child", 0.01)
        with mock.patch.object(CHECK.os, "killpg") as kill:
            with self.assertRaisesRegex(ValueError, "git_reap_timeout"):
                CHECK.stop_git_query(process)
        kill.assert_called_once_with(process.pid, CHECK.signal.SIGKILL)
        process.wait.assert_called_once_with(timeout=CHECK.GIT_REAP_TIMEOUT_SECONDS)
        process.reset_mock()
        process.wait.side_effect = None
        process.wait.return_value = 0
        with mock.patch.object(CHECK.os, "killpg", side_effect=PermissionError):
            with self.assertRaisesRegex(ValueError, "git_termination_failed"):
                CHECK.stop_git_query(process)
        process.wait.assert_called_once_with(timeout=CHECK.GIT_REAP_TIMEOUT_SECONDS)

    def test_history_accepts_public_baseline_and_upgrade_with_remote(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            repo.commit(public_fixture())
            repo.git("remote", "add", "origin", "https://example.invalid/public.git")
            self.assertEqual(CHECK.history_findings(repo.root, baseline=baseline), [{}, {}])

    def test_history_rejects_deleted_file_leak(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            files = public_fixture()
            private = "/" + "home" + "/example/synthetic-private"
            repo.commit(files | {"old-record.json": json.dumps({"p": private}).encode()})
            repo.commit(files)
            self.assertFalse(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.REPO_FILES))
            self.assertTrue(any(CHECK.history_findings(repo.root, baseline=baseline)))

    def test_history_rejects_encoded_leak_removed_from_allowed_file(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            files = public_fixture()
            private = "/" + "home" + "/example/synthetic-private"
            encoded = base64.b64encode(private.encode().hex().encode())
            repo.commit(files | {"README.md": encoded})
            repo.commit(files)
            rows = CHECK.history_findings(repo.root, baseline=baseline)
            self.assertTrue(rows[1]["README.md"]["private_path"])
            self.assertFalse(rows[2])

    def test_history_rejects_metadata_leak(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            private = "/" + "home" + "/example/synthetic-private"
            repo.commit(public_fixture(), base64.b64encode(private.encode()).decode())
            repo.commit(public_fixture())
            rows = CHECK.history_findings(repo.root, baseline=baseline)
            self.assertTrue(rows[1]["commit-metadata"]["private_path"])

    def test_history_rejects_unapproved_root_and_accepts_ancestor_second_parent(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            main = repo.commit(public_fixture())
            with self.assertRaises(ValueError):
                CHECK.history_findings(repo.root)
            tree = repo.git("rev-parse", "HEAD^{tree}")
            for parents in ((main, baseline), (baseline, main)):
                merge = repo.commit_tree(tree, parents, "Synthetic ancestor merge " + parents[0])
                repo.git("update-ref", "HEAD", merge)
                self.assertEqual(CHECK.history_findings(repo.root, baseline=baseline), [{}, {}, {}])

    def test_history_complete_diamonds_ignore_dates_and_scan_each_unique_commit(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            tree = repo.stage(public_fixture())
            commits, tip = [baseline], baseline
            for layer in range(3):
                left = repo.commit_tree(tree, [tip], f"left {layer}", date="2030-01-01T00:00:00+0000")
                right = repo.commit_tree(tree, [tip], f"right {layer}", date="2000-01-01T00:00:00+0000")
                tip = repo.commit_tree(tree, [left, right], f"diamond {layer}", date="1990-01-01T00:00:00+0000")
                commits.extend((left, right, tip))
            joined = repo.commit_tree(tree, [tip, right], "consecutive merge")
            commits.append(joined)
            repo.git("update-ref", "HEAD", joined)
            actual = CHECK.git
            scanned = []
            def record(root, *args, **kwargs):
                if args[:2] == ("cat-file", "commit"):
                    scanned.append(args[2])
                return actual(root, *args, **kwargs)
            with mock.patch.object(CHECK, "git", side_effect=record), \
                 mock.patch.object(CHECK, "tree_files", wraps=CHECK.tree_files) as trees:
                rows = CHECK.history_findings(repo.root, baseline=baseline)
            self.assertEqual(rows, [{} for _ in commits])
            self.assertEqual(CHECK.Counter(scanned), CHECK.Counter(commits))
            self.assertEqual(trees.call_count, len(commits))

    def test_history_both_branches_deleted_canaries_survive_a_clean_merge(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            files = public_fixture()
            private = ("/" + "home/example/dag-canary").encode()
            clean_tree = repo.stage(files)
            left_tree = repo.stage(files | {"deleted-canary.json": private})
            left = repo.commit_tree(left_tree, [baseline], "left before deletion")
            left_clean = repo.commit_tree(clean_tree, [left], "left after deletion")
            right_tree = repo.stage(files | {"README.md": base64.b64encode(private + b"\0")})
            right = repo.commit_tree(right_tree, [baseline], "right before deletion")
            right_clean = repo.commit_tree(clean_tree, [right], "right after deletion")
            merged = repo.commit_tree(clean_tree, [left_clean, right_clean], "clean merge")
            repo.git("update-ref", "HEAD", merged)
            rows = CHECK.history_findings(repo.root, baseline=baseline)
            self.assertTrue(any(row.get("deleted-canary.json", {}).get("unexpected_file") for row in rows))
            self.assertTrue(any(row.get("README.md", {}).get("private_path") for row in rows))
            self.assertFalse(rows[-1])
            self.assertEqual(len(rows), 6)

    def test_history_same_tree_keeps_both_branch_and_merge_metadata(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            tree = repo.stage(public_fixture())
            private = ("/" + "home/example/metadata-canary").encode()
            credential = ("gh" + "p_" + "A" * 30).encode()
            left = repo.commit_tree(tree, [baseline], base64.b64encode(private).decode())
            right = repo.commit_tree(tree, [baseline], base64.b64encode(credential + b"\0").decode())
            merge = repo.commit_tree(tree, [left, right], base64.b64encode(private.hex().encode()).decode())
            repo.git("update-ref", "HEAD", merge)
            rows = CHECK.history_findings(repo.root, baseline=baseline)
            self.assertEqual(sum("commit-metadata" in row for row in rows), 3)
            self.assertEqual(sum(bool(row.get("commit-metadata", {}).get("private_path")) for row in rows), 2)
            self.assertEqual(sum(bool(row.get("commit-metadata", {}).get("credential_shape")) for row in rows), 1)

    def test_history_second_root_is_rejected(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            tree = repo.stage(public_fixture())
            unrelated = repo.commit_tree(tree, [], "unrelated root")
            merge = repo.commit_tree(tree, [baseline, unrelated], "unrelated merge")
            repo.git("update-ref", "HEAD", merge)
            with self.assertRaisesRegex(ValueError, "unexpected_history_root"):
                CHECK.history_findings(repo.root, baseline=baseline)

    def test_history_missing_objects_and_noncommit_parents_fail_closed(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            tree = repo.stage(public_fixture())
            ancestor = repo.commit_tree(tree, [baseline], "deep parent")
            left = repo.commit_tree(tree, [ancestor], "left")
            right = repo.commit_tree(tree, [ancestor], "right")
            merge = repo.commit_tree(tree, [left, right], "complete merge")
            repo.git("update-ref", "HEAD", merge)
            blob = repo.git("rev-parse", tree + ":README.md")
            for label, oid in (("first-parent", left), ("second-parent", right),
                               ("deep-ancestor", ancestor), ("root", baseline), ("tree", tree), ("blob", blob)):
                with self.subTest(missing=label):
                    target = repo.object_path(oid)
                    saved = target.read_bytes()
                    target.unlink()
                    try:
                        with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                            CHECK.history_findings(repo.root, baseline=baseline)
                    finally:
                        target.write_bytes(saved)
            for parent in (blob, tree):
                with self.subTest(noncommit=parent):
                    malformed = repo.raw_commit(tree, [baseline, parent])
                    # Bypass ref validation only in this malformed-object fixture;
                    # the scanner must reject the graph, not its constructor.
                    (repo.root / ".git/HEAD").write_text(malformed + "\n")
                    with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                        CHECK.history_findings(repo.root, baseline=baseline)

    def test_history_ancestor_modes_cannot_be_hidden_by_cleaning_a_merge(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            files = public_fixture()
            clean = repo.stage(files)
            normal = repo.commit_tree(clean, [baseline], "normal side")
            for mode in ("120000", "160000", "100755"):
                with self.subTest(mode=mode):
                    repo.stage(files)
                    oid = baseline if mode == "160000" else repo.git("rev-parse", clean + ":README.md")
                    repo.git("update-index", "--cacheinfo", mode + "," + oid + ",README.md")
                    bad = repo.commit_tree(repo.git("write-tree"), [baseline], "unsafe ancestor " + mode)
                    repaired = repo.commit_tree(clean, [bad], "cleaned ancestor " + mode)
                    merged = repo.commit_tree(clean, [normal, repaired], "clean merged tree " + mode)
                    repo.git("update-ref", "HEAD", merged)
                    with self.assertRaisesRegex(ValueError, "unsafe_git_entry"):
                        CHECK.history_findings(repo.root, baseline=baseline)

    def test_history_new_merge_reusing_legacy_tree_gets_no_schema_grant(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            legacy_tree = repo.git("rev-parse", baseline + "^{tree}")
            current_tree = repo.stage(public_fixture())
            child = repo.commit_tree(current_tree, [baseline], "current child")
            merge = repo.commit_tree(legacy_tree, [baseline, child], "new OID old tree")
            repo.git("update-ref", "HEAD", merge)
            rows = CHECK.history_findings(repo.root, baseline=baseline)
            self.assertGreater(rows[-1]["Cargo.toml"]["manifest_mismatch"], 0)
            self.assertGreater(rows[-1]["src/capture/owner.rs"]["missing_allowed_file"], 0)

    def test_history_commit_parent_and_work_budgets_have_exact_boundaries(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            tree = repo.stage(public_fixture())
            left = repo.commit_tree(tree, [baseline], "left budget")
            right = repo.commit_tree(tree, [baseline], "right budget")
            merge = repo.commit_tree(tree, [left, right], "merge budget")
            tip = repo.commit_tree(tree, [merge, right], "joined budget")
            repo.git("update-ref", "HEAD", tip)
            for constant, boundary, rule in (("HISTORY_MAX_COMMITS", 5, "history_commit_budget_exceeded"),
                                             ("HISTORY_MAX_PARENT_EDGES", 6, "history_parent_budget_exceeded")):
                with self.subTest(budget=constant):
                    with mock.patch.object(CHECK, constant, boundary):
                        self.assertEqual(CHECK.history_findings(repo.root, baseline=baseline), [{}] * 5)
                    with mock.patch.object(CHECK, constant, boundary - 1):
                        with self.assertRaisesRegex(ValueError, rule):
                            CHECK.history_findings(repo.root, baseline=baseline)
            actual, sizes = CHECK.git, []
            def account(root, *args, **kwargs):
                result = actual(root, *args, **kwargs)
                sizes.append(len(result))
                return result
            with mock.patch.object(CHECK, "git", side_effect=account):
                self.assertEqual(CHECK.history_findings(repo.root, baseline=baseline), [{}] * 5)
            query_boundary = len(sizes)
            with mock.patch.object(CHECK, "HISTORY_MAX_QUERIES", query_boundary):
                self.assertEqual(CHECK.history_findings(repo.root, baseline=baseline), [{}] * 5)
            with mock.patch.object(CHECK, "HISTORY_MAX_QUERIES", query_boundary - 1):
                with self.assertRaisesRegex(ValueError, "history_query_budget_exceeded"):
                    CHECK.history_findings(repo.root, baseline=baseline)
            boundary = sum(sizes)
            with mock.patch.object(CHECK, "HISTORY_MAX_WORK_BYTES", boundary):
                self.assertEqual(CHECK.history_findings(repo.root, baseline=baseline), [{}] * 5)
            with mock.patch.object(CHECK, "HISTORY_MAX_WORK_BYTES", boundary - 1):
                with self.assertRaisesRegex(ValueError, "history_work_budget_exceeded"):
                    CHECK.history_findings(repo.root, baseline=baseline)

    def test_history_pins_head_and_disables_lazy_fetch_for_every_query(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            tree = repo.stage(public_fixture())
            head = repo.commit_tree(tree, [baseline], "pinned head")
            later = repo.commit_tree(tree, [head], "later unpinned child")
            repo.git("update-ref", "HEAD", head)
            actual, scanned, moved = CHECK.git, [], False
            def move_after_pin(root, *args, **kwargs):
                nonlocal moved
                result = actual(root, *args, **kwargs)
                if args == ("rev-parse", "--verify", "HEAD") and not moved:
                    repo.git("update-ref", "HEAD", later)
                    moved = True
                if args[:2] == ("cat-file", "commit"):
                    scanned.append(args[2])
                return result
            with mock.patch.object(CHECK, "git", side_effect=move_after_pin):
                self.assertEqual(CHECK.history_findings(repo.root, baseline=baseline), [{}, {}])
            self.assertEqual(scanned, [baseline, head])
            with mock.patch.object(CHECK.subprocess, "Popen", wraps=CHECK.subprocess.Popen) as processes:
                CHECK.history_findings(repo.root, baseline=baseline)
            self.assertTrue(processes.call_args_list)
            self.assertTrue(all(call.kwargs["env"]["GIT_NO_LAZY_FETCH"] == "1" for call in processes.call_args_list))

    def test_history_shallow_and_grafts_are_still_rejected(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            for target, contents in ((repo.root / ".git/shallow", baseline + "\n"),
                                     (repo.root / ".git/info/grafts", "")):
                with self.subTest(view=target.name):
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(contents)
                    try:
                        with self.assertRaisesRegex(ValueError, "incomplete_or_rewritten_history_view"):
                            CHECK.history_findings(repo.root, baseline=baseline)
                    finally:
                        target.unlink()

    def test_history_rejects_replace_refs(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            clean = repo.commit(public_fixture())
            repo.commit(public_fixture(), "Synthetic later revision")
            repo.git("replace", "HEAD", clean)
            with self.assertRaises(ValueError):
                CHECK.history_findings(repo.root, baseline=baseline)

    def test_archive_accepts_clean_commit_and_rejects_dirty_or_wrong_origin(self):
        with repository() as (repo, archive):
            files = public_fixture()
            commit = repo.commit(files)
            vcs = {"git": {"sha1": commit}, "path_in_vcs": ""}
            write_archive(archive, files, vcs=vcs)
            self.assertFalse(CHECK.archive_findings(repo.root, archive)[1])
            for bad in ({"git": {"sha1": commit, "dirty": True}, "path_in_vcs": ""},
                        {"git": {"sha1": "0" * 40}, "path_in_vcs": ""}):
                write_archive(archive, files, vcs=bad)
                with self.assertRaises(ValueError):
                    CHECK.archive_findings(repo.root, archive)
            write_archive(archive, files, vcs=vcs)
            (repo.root / "README.md").write_bytes(b"uncommitted public text")
            with self.assertRaises(ValueError):
                CHECK.archive_findings(repo.root, archive)

    def test_candidate_archive_requires_exact_tree_and_source(self):
        with repository() as (repo, archive):
            files = public_fixture()
            repo.commit(public_fixture(legacy=True))
            tree = repo.stage(files)
            write_archive(archive, files)
            self.assertFalse(CHECK.archive_findings(repo.root, archive, candidate_tree=tree)[1])
            clean = archive.read_bytes()
            basename = ("RunDelta-" + CHECK.VERSION + ".crate").encode() + b"\0"
            archive.write_bytes(clean[:3] + bytes([8]) + clean[4:10] + basename + clean[10:])
            self.assertFalse(CHECK.archive_findings(repo.root, archive, candidate_tree=tree)[1])
            with self.assertRaises((ValueError, KeyError)):
                CHECK.archive_findings(repo.root, archive)
            for extra in ({"docs/CRATE_README.md": b"different public text"},
                          {"unknown.txt": b"synthetic"},
                          {"Cargo.toml": files["Cargo.toml"] + b"\n[dependencies]\nunknown = \"1\"\n"}):
                write_archive(archive, files, extra=extra)
                if "unknown.txt" in extra:
                    with self.assertRaisesRegex(ValueError, "unsafe_archive_entry"):
                        CHECK.archive_findings(repo.root, archive, candidate_tree=tree)
                else:
                    self.assertTrue(CHECK.archive_findings(repo.root, archive, candidate_tree=tree)[1])

    def test_malformed_archive_metadata_and_manifest_fail_without_traceback(self):
        with repository() as (repo, archive):
            files = public_fixture()
            commit = repo.commit(files)
            vcs = {"git": {"sha1": commit}, "path_in_vcs": ""}
            for extra in ({".cargo_vcs_info.json": b'[]'},
                          {".cargo_vcs_info.json": b'{"git":[],"path_in_vcs":""}'},
                          {".cargo_vcs_info.json": b'not json'},
                          {"Cargo.toml": b'package = "not a table"\n'}):
                write_archive(archive, files, vcs=vcs, extra=extra)
                result = subprocess.run(["python3", "-B", str(ROOT / "tools/check_export.py"),
                                         "--root", str(repo.root), "--archive", str(archive)],
                                        text=True, capture_output=True)
                self.assertEqual(result.returncode, 1)
                self.assertFalse(json.loads(result.stdout)["passed"])
                self.assertNotIn("Traceback", result.stderr)

    def test_archive_rejects_unsafe_members(self):
        with repository() as (_, archive):
            for name, kind in (("../escape", tarfile.REGTYPE),
                               ("RunDelta-" + CHECK.VERSION + "/link", tarfile.SYMTYPE)):
                buffer = io.BytesIO()
                with tarfile.open(fileobj=buffer, mode="w") as handle:
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    member.linkname = "LICENSE" if kind == tarfile.SYMTYPE else ""
                    handle.addfile(member, io.BytesIO())
                archive.write_bytes(gzip.compress(buffer.getvalue(), mtime=0))
                with self.assertRaisesRegex(ValueError, "unsafe_archive_entry"):
                    CHECK.archive_files(archive)

    def test_archive_rejects_hidden_container_metadata_and_trailers(self):
        with repository() as (repo, archive):
            files = public_fixture()
            tree = repo.stage(files)
            private = "/" + "home" + "/example/synthetic-private"
            for kwargs in ({"pax_headers": {"comment": private}}, {"uname": private}):
                write_archive(archive, files, **kwargs)
                with self.assertRaises(ValueError):
                    CHECK.archive_findings(repo.root, archive, candidate_tree=tree)
            write_archive(archive, files)
            clean = archive.read_bytes()
            for changed in (clean + private.encode(), clean + gzip.compress(private.encode()),
                            gzip.compress(gzip.decompress(clean) + private.encode())):
                archive.write_bytes(changed)
                with self.assertRaises(ValueError):
                    CHECK.archive_findings(repo.root, archive, candidate_tree=tree)
            expanded = bytearray(gzip.decompress(clean))
            with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as handle:
                member = next(m for m in handle if m.size % 512)
            expanded[member.offset_data + member.size] = ord("X")
            archive.write_bytes(gzip.compress(expanded))
            with self.assertRaises(ValueError):
                CHECK.archive_findings(repo.root, archive, candidate_tree=tree)
            for flag in (8, 16):
                # Gzip FNAME and FCOMMENT are NUL-terminated optional fields.
                changed = clean[:3] + bytes([flag]) + clean[4:10] + private.encode() + b"\0" + clean[10:]
                archive.write_bytes(changed)
                with self.assertRaises(ValueError):
                    CHECK.archive_findings(repo.root, archive, candidate_tree=tree)

    def test_diagnostics_do_not_disclose_unallowed_filename(self):
        with repository() as (repo, _):
            repo.commit(public_fixture())
            secret = "gh" + "p_" + "a" * 30
            (repo.root / secret).write_bytes(b"synthetic")
            result = subprocess.run(["python3", "-B", str(ROOT / "tools/check_export.py"),
                                     "--root", str(repo.root)], text=True, capture_output=True)
            self.assertEqual(result.returncode, 1)
            self.assertNotIn(secret, result.stdout + result.stderr)
            self.assertIn("<unallowed-file>", result.stdout)


class WorkflowSafety(unittest.TestCase):
    """Source-only Actions checks; ordinary crate tests need no workflow/tool."""

    actionlint = "actionlint"

    def lint(self, source):
        # Context availability is GitHub-specific, not a YAML syntax rule.
        # Disable optional Python/shell linters so this check has one dependency.
        return subprocess.run([self.actionlint, "-shellcheck=", "-pyflakes=", "-"],
                              input=source, text=True, capture_output=True,
                              cwd=ROOT, timeout=30)

    def test_checked_in_workflow_has_valid_actions_contexts(self):
        source = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        result = self.lint(source)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_runner_context_is_rejected_at_job_env(self):
        source = """on: workflow_dispatch
jobs:
  synthetic:
    runs-on: ubuntu-24.04
    env:
      CARGO_TARGET_DIR: ${{ runner.temp }}/rundelta-build
    steps:
      - run: 'true'
"""
        rejected = self.lint(source)
        self.assertEqual(rejected.returncode, 1, rejected.stdout + rejected.stderr)
        self.assertIn('context "runner" is not allowed', rejected.stdout + rejected.stderr)
        # The same expression is valid at step scope. This prevents a blanket
        # text ban from masquerading as context-aware validation.
        accepted = source.replace(
            "    env:\n      CARGO_TARGET_DIR: ${{ runner.temp }}/rundelta-build\n"
            "    steps:\n      - run: 'true'\n",
            "    steps:\n      - run: 'true'\n"
            "        env:\n          CARGO_TARGET_DIR: ${{ runner.temp }}/rundelta-build\n")
        checked = self.lint(accepted)
        self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workflow", metavar="ACTIONLINT",
                        help="run source-only workflow regressions using this actionlint executable")
    args = parser.parse_args()
    WorkflowSafety.actionlint = args.workflow or "actionlint"
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        WorkflowSafety if args.workflow else ExportSafety)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() and result.testsRun and not (
        result.skipped or result.expectedFailures) else 1)
