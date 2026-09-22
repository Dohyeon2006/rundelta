#!/usr/bin/env python3
"""Synthetic public-export counterexamples, also runnable from the crate."""
import base64
from contextlib import contextmanager
import gzip
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
import urllib.parse

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("check_export", ROOT / "tools/check_export.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def public_fixture(*, legacy=False):
    """Use synthetic bodies, never real execution records or personal data."""
    payload = CHECK.LEGACY_PAYLOAD if legacy else CHECK.PAYLOAD
    names = CHECK.LEGACY_WORKTREE if legacy else CHECK.WORKTREE
    files = {name: b"synthetic public fixture\n" for name in names}
    manifest = ["[package]", 'name = "RunDelta"',
                "version = " + json.dumps("0.1.0" if legacy else CHECK.VERSION),
                'license = "MIT"', 'readme = "README.md"',
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
        self.paths = set(files)
        self.git("add", "--all")
        return self.git("write-tree")

    def commit(self, files, message="Synthetic public revision"):
        self.stage(files)
        self.git("commit", "--quiet", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")


@contextmanager
def repository():
    with tempfile.TemporaryDirectory(prefix="rundelta-export-test-") as directory:
        root = Path(directory) / "checkout"
        root.mkdir()
        yield SyntheticGit(root), Path(directory) / "candidate.crate"


def write_archive(path, files, *, vcs=None, extra=None, pax_headers=None, uname=""):
    packaged = {name: files[name] for name in CHECK.PAYLOAD}
    packaged["Cargo.toml.orig"] = files["Cargo.toml"]
    if vcs is not None:
        packaged[".cargo_vcs_info.json"] = json.dumps(vcs).encode()
    packaged.update(extra or {})
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", pax_headers=pax_headers) as archive:
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
        files = {name: (ROOT / name).read_bytes() for name in CHECK.PAYLOAD}
        if (ROOT / "Cargo.toml.orig").exists():
            files["Cargo.toml"] = (ROOT / "Cargo.toml.orig").read_bytes()
        self.assertFalse(CHECK.inspect_files(files, CHECK.PAYLOAD))

    def test_inventory_rejects_extra_missing_fixture_and_wrong_license(self):
        files = public_fixture()
        self.assertFalse(CHECK.inspect_files(files, CHECK.WORKTREE))
        for name in ("target/program", "docs/" + "external-" + "alpha/data.json",
                     "." + "idea/config", "unexpected.txt"):
            self.assertTrue(CHECK.inspect_files(files | {name: b"synthetic"}, CHECK.WORKTREE))
        for fixture in ("tests/fixtures/trace.txt", "tests/fixtures/finalization_barrier.c"):
            missing = {k: v for k, v in files.items() if k != fixture}
            self.assertTrue(CHECK.inspect_files(missing, CHECK.WORKTREE))
        self.assertTrue(CHECK.inspect_files(files | {"LICENSE": b"incorrect"}, CHECK.WORKTREE))

    def test_wrong_cargo_include_and_duplicate_entry_fail(self):
        files = public_fixture()
        manifest = files["Cargo.toml"]
        for changed in (manifest.replace(b'"/tests/fixtures/trace.txt", ', b""),
                        manifest.replace(b'include = [', b'include = ["/README.md", ')):
            self.assertNotEqual(changed, manifest)
            self.assertTrue(CHECK.inspect_files(files | {"Cargo.toml": changed}, CHECK.WORKTREE))

    def test_workflow_is_required_source_only_and_not_a_privacy_exception(self):
        name = ".github/workflows/ci.yml"
        files = public_fixture()
        self.assertIn(name, CHECK.WORKTREE)
        self.assertNotIn(name, CHECK.PAYLOAD)
        self.assertTrue(CHECK.inspect_files({k: v for k, v in files.items() if k != name},
                                            CHECK.WORKTREE))
        private = "/" + "home" + "/example/synthetic-private"
        for data in (private.encode(), base64.b64encode(private.encode())):
            self.assertTrue(CHECK.inspect_files(files | {name: data}, CHECK.WORKTREE))
        manifest = files["Cargo.toml"].replace(
            b'include = [', b'include = ["/.github/workflows/ci.yml", ')
        self.assertTrue(CHECK.inspect_files(files | {"Cargo.toml": manifest}, CHECK.WORKTREE))
        with repository() as (repo, archive):
            tree = repo.stage(files)
            write_archive(archive, files, extra={name: b"synthetic workflow"})
            self.assertTrue(CHECK.archive_findings(repo.root, archive, candidate_tree=tree)[1])

    def test_worktree_scans_ignored_and_untracked_files(self):
        with repository() as (repo, _):
            files = public_fixture()
            files[".gitignore"] = b"ignored/\n"
            repo.commit(files)
            self.assertFalse(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.WORKTREE))
            (repo.root / "ignored").mkdir()
            (repo.root / "ignored" / "secret").write_bytes(b"synthetic")
            self.assertTrue(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.WORKTREE))

    def test_worktree_rejects_empty_output_directory(self):
        with repository() as (repo, _):
            repo.commit(public_fixture())
            (repo.root / "target").mkdir()
            self.assertTrue(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.WORKTREE))

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
            self.assertFalse(CHECK.inspect_files(CHECK.worktree_files(repo.root), CHECK.WORKTREE))
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

    def test_history_rejects_unapproved_root_and_merge(self):
        with repository() as (repo, _):
            baseline = repo.commit(public_fixture(legacy=True))
            repo.commit(public_fixture())
            with self.assertRaises(ValueError):
                CHECK.history_findings(repo.root)
            main = repo.git("rev-parse", "HEAD")
            tree = repo.git("rev-parse", "HEAD^{tree}")
            merge = repo.git("commit-tree", tree, "-p", main, "-p", baseline, "-m", "Synthetic merge")
            repo.git("update-ref", "HEAD", merge)
            with self.assertRaises(ValueError):
                CHECK.history_findings(repo.root, baseline=baseline)

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
            for extra in ({"README.md": b"different public text"},
                          {"unknown.txt": b"synthetic"},
                          {"Cargo.toml": files["Cargo.toml"] + b"\n[dependencies]\nunknown = \"1\"\n"}):
                write_archive(archive, files, extra=extra)
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
