#!/usr/bin/env python3
"""Read-only public-export closure checks; diagnostics never disclose match values."""
import argparse
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
PAYLOAD = LEGACY_PAYLOAD | {
    "docs/ARCHITECTURE.md", "docs/CHANGELOG.md",
    "tools/check_export.py", "tests/test_export_safety.py",
    "tests/fixtures/finalization_barrier.c",
}
WORKTREE = PAYLOAD | {".gitignore", ".github/workflows/ci.yml"}
ARCHIVE = PAYLOAD | {"Cargo.toml.orig", ".cargo_vcs_info.json"}
LICENSE_SHA = "f8e9acc75d2b3ad143bedc06b425525f311dd51b9b0cb1885dff81e1fc200a7b"
PRIVATE_PATH = re.compile(
    r"/(?:home|Users|root|tmp|var/tmp|var/folders|private/tmp|private/var/folders|run/user)/"
    r"|[A-Za-z]:[\\/]Users[\\/]", re.I)
MATERIAL = re.compile(
    r"(?i)(?:docs[/\\](?:external[-]alpha|external[-]cases|performance|noise[-]folding)"
    r"|(?:^|[/\\])\.idea(?:[/\\]|$)|(?:^|[/\\])AGENTS\.md(?:$|[\s\"'])"
    r"|(?:ACCEPTANCE|ARCHITECTURE_REVIEW|FIELD_CASES|HOLDOUT_RESULTS|RANKING_REPAIR"
    r"|REFACTOR_STATUS|REFACTOR_DECISIONS)\.(?:md|json))")
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
B64 = re.compile(r"(?<![A-Za-z0-9_+/=-])[A-Za-z0-9_+/-]{12,}={0,2}(?![A-Za-z0-9_+/=-])")
WRAPPED_B64 = re.compile(r"(?m)^[A-Za-z0-9_+/-]{4,}={0,2}(?:\r?\n[A-Za-z0-9_+/-]{4,}={0,2})+$")


def decode_escapes(text):
    text = text.replace(r"\/", "/").replace("\\\\", "\\").replace(r'\"', '"')

    def char(match):
        value = match[1]
        code = int(value[1:], 16) if value[0] in "xuU" else int(value, 8)
        return chr(code) if code <= 0x10FFFF else "?"

    text = re.sub(r"(?<!\\)\\(x[0-9a-fA-F]{2}|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|[0-7]{1,3})", char, text)
    return html.unescape(urllib.parse.unquote(text))


def privacy(name, data, private_prefixes=(), *, legacy=False):
    """Traverse decoded views with a fail-closed expansion limit."""
    counts = Counter()
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return Counter({"non_utf8_source": 1})
    queue = deque([(text, 0)])
    seen = set()
    constants = LEGACY_CONSTANTS if legacy else CONSTANTS
    while queue:
        view, depth = queue.popleft()
        if view in seen:
            continue
        if depth > 12 or len(seen) >= 20_000:
            counts["encoding_budget_exceeded"] += 1
            break
        seen.add(view)
        filtered = view
        for value in constants.get(name, ()):
            filtered = re.sub(r'\\*"' + re.escape(value) + r'\\*"', '"reviewed-constant"', filtered)
        counts["private_path"] += len(PRIVATE_PATH.findall(filtered))
        counts["excluded_material"] += len(MATERIAL.findall(filtered))
        counts["credential_shape"] += len(SECRET.findall(filtered))
        counts["explicit_private_prefix"] += sum(filtered.count(p) for p in private_prefixes if p)
        decoded = decode_escapes(view)
        if decoded != view:
            queue.append((decoded, depth + 1))
        for match in WRAPPED_B64.finditer(view):
            queue.append(("".join(match[0].splitlines()), depth + 1))
        for match in HEX.finditer(view):
            try:
                queue.append((bytes.fromhex(match[0]).decode("utf-8"), depth + 1))
            except UnicodeDecodeError:
                pass
        for match in B64.finditer(view):
            token = match[0]
            try:
                value = base64.b64decode(token + "=" * (-len(token) % 4), altchars=b"-_", validate=True)
                decoded = value.decode("utf-8")
            except (ValueError, UnicodeDecodeError):
                continue
            if decoded:
                queue.append((decoded, depth + 1))
    return +counts


def inspect_files(files, expected, private_prefixes=(), *, legacy=False):
    findings = {}
    payload = LEGACY_PAYLOAD if legacy else PAYLOAD
    for name in sorted(set(files) | expected):
        if name not in files:
            findings[name] = Counter({"missing_allowed_file": 1})
        elif name not in expected:
            findings[name] = Counter({"unexpected_file": 1})
        else:
            counts = privacy(name, files[name], private_prefixes, legacy=legacy)
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
                     and pkg["license"] == "MIT" and pkg["readme"] == "README.md"
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
        elif path.is_dir() and not any(n.startswith(name + "/") for n in WORKTREE):
            files[name + "/"] = b""
        elif not path.is_dir():
            raise ValueError("nonregular_worktree_entry")
    return files


def tree_files(root, revision):
    files = {}
    for row in git(root, "ls-tree", "-rz", "--full-tree", revision).split(b"\0"):
        if not row:
            continue
        meta, name = row.split(b"\t", 1)
        mode, kind, oid = meta.split()
        if mode != b"100644" or kind != b"blob":
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
        files = tree_files(root, commit)
        legacy = commit == baseline
        findings = inspect_files(files, LEGACY_WORKTREE if legacy else WORKTREE,
                                 private_prefixes, legacy=legacy)
        metadata = privacy("commit-metadata", git(root, "cat-file", "commit", commit), private_prefixes)
        if metadata:
            findings["commit-metadata"] = metadata
        rows.append(findings)
        previous = commit
    return rows


def archive_files(archive):
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
            if privacy("archive-metadata", expanded[member.offset:member.offset_data]):
                raise ValueError("private_archive_metadata")
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
                           for name in PAYLOAD if name.startswith("tests/") and name.endswith(".rs")),
                          key=lambda entry: (entry["name"], entry["path"]))
        if normalized["test"] == expected:
            normalized.pop("test")
    return original == normalized


def archive_findings(root, archive, private_prefixes=(), *, candidate_tree=None):
    files = archive_files(archive)
    expected = ARCHIVE - ({".cargo_vcs_info.json"} if candidate_tree else set())
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
    for name in PAYLOAD - {"Cargo.toml"}:
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
                safe_name = name if name in ARCHIVE | WORKTREE | {"commit-metadata"} else "<unallowed-file>"
                findings[(scope, safe_name, rule)] += count

    files = worktree_files(args.root)
    add("worktree", inspect_files(files, WORKTREE, args.private_prefix))
    inspected["worktree_files"] = len(files)
    if args.candidate_tree:
        if (not re.fullmatch(r"[0-9a-f]{40}", args.candidate_tree)
                or git(args.root, "cat-file", "-t", args.candidate_tree).strip() != b"tree"):
            raise ValueError("invalid_candidate_tree")
        candidate = tree_files(args.root, args.candidate_tree)
        add("candidate", inspect_files(candidate, WORKTREE, args.private_prefix))
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
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError, tarfile.TarError, zlib.error):
        print(json.dumps({"passed": False, "rule": "validation_error", "count": 1}))
        raise SystemExit(1)
