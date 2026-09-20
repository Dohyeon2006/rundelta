#!/usr/bin/env python3
"""Read-only public export checks. Diagnostics never include matched values."""
import argparse
import base64
from collections import Counter, deque
import hashlib
import html
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib
import urllib.parse

SOURCES = {
    "src/main.rs", "src/cli.rs", "src/capture/mod.rs", "src/parse/mod.rs",
    "src/parse/fs_tests.rs", "src/model/mod.rs", "src/store/mod.rs",
    "src/diff/mod.rs", "src/report/mod.rs",
}
PAYLOAD = SOURCES | {
    "Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "assets/rundelta-signal.svg",
    "docs/BUILDING.md", "docs/PROJECT.md",
    "tests/fixtures/trace.txt", "tests/public_cli.rs", "tests/public_cli.py",
}
WORKTREE = PAYLOAD | {
    ".gitignore", "tools/check_export.py", "tests/test_export_safety.py",
}
ARCHIVE = PAYLOAD | {"Cargo.toml.orig", ".cargo_vcs_info.json"}
LICENSE_SHA = "f8e9acc75d2b3ad143bedc06b425525f311dd51b9b0cb1885dff81e1fc200a7b"
PRIVATE_PATH = re.compile(
    r"/(?:home|Users|root|tmp|var/tmp|var/folders|private/tmp|private/var/folders|run/user)/"
    r"|[A-Za-z]:[\\/]Users[\\/]", re.I)
MATERIAL = re.compile(
    r"(?i)(?:docs[/\\](?:external[-]alpha|external[-]cases|performance|noise[-]folding)"
    r"|(?:^|[/\\])\.idea(?:[/\\]|$)"
    r"|(?:ACCEPTANCE|ARCHITECTURE_REVIEW|FIELD_CASES|HOLDOUT_RESULTS|RANKING_REPAIR)\.(?:md|json))")
SECRET = re.compile(
    r"gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}"
    r"|AKIA[0-9A-Z]{16}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"
    r"|https?://[^\s/@:]+:[^\s/@]+@")
TEMP_ROOT = chr(47) + "tmp" + chr(47)
CONSTANTS = {
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


def privacy(name, data, private_prefixes=()):
    """Return rule counts only; traverse encoded views with bounded expansion."""
    counts = Counter()
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return Counter({"non_utf8_source": 1})
    queue = deque([(text, 0)])
    seen = set()
    while queue:
        view, depth = queue.popleft()
        if view in seen:
            continue
        if depth > 12 or len(seen) >= 20_000:
            counts["encoding_budget_exceeded"] += 1
            break
        seen.add(view)
        filtered = view
        for value in CONSTANTS.get(name, ()):
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


def inspect_files(files, expected, private_prefixes=()):
    findings = {}
    for name in sorted(set(files) | expected):
        if name not in files:
            findings[name] = Counter({"missing_allowed_file": 1})
        elif name not in expected:
            findings[name] = Counter({"unexpected_file": 1})
        else:
            counts = privacy(name, files[name], private_prefixes)
            if counts:
                findings[name] = counts
    if files.get("LICENSE") and hashlib.sha256(files["LICENSE"]).hexdigest() != LICENSE_SHA:
        findings.setdefault("LICENSE", Counter())["license_mismatch"] += 1
    for name in ("Cargo.toml", "Cargo.toml.orig"):
        if name not in files:
            continue
        try:
            pkg = tomllib.loads(files[name].decode())["package"]
            valid = (pkg["name"] == "RunDelta" and pkg["version"] == "0.1.0"
                     and pkg["license"] == "MIT" and pkg["readme"] == "README.md"
                     and pkg["repository"] == "https://github.com/Dohyeon2006/rundelta"
                     and set(pkg["include"]) == {"/" + n for n in PAYLOAD}
                     and not any(pkg.get(k) for k in ("homepage", "documentation")))
        except (KeyError, ValueError):
            valid = False
        if not valid:
            findings.setdefault(name, Counter())["manifest_mismatch"] += 1
    return findings


def git(root, *args):
    return subprocess.check_output(["git", "--no-optional-locks", "-C", str(root), *args], stderr=subprocess.PIPE)


def worktree_files(root):
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
    return files


def history_files(root):
    for commit in git(root, "rev-list", "--all").decode().splitlines():
        files = {}
        for row in git(root, "ls-tree", "-rz", "--full-tree", commit).split(b"\0"):
            if not row:
                continue
            meta, name = row.split(b"\t", 1)
            mode, kind, oid = meta.split()
            if mode != b"100644" or kind != b"blob":
                raise ValueError("unsafe_git_entry")
            files[name.decode()] = git(root, "cat-file", "blob", oid.decode())
        yield files, git(root, "cat-file", "commit", commit)


def archive_files(archive):
    files = {}
    with tarfile.open(archive) as handle:
        for member in handle.getmembers():
            path = PurePosixPath(member.name)
            if (not member.isfile() or path.is_absolute() or ".." in path.parts
                    or not path.parts or path.parts[0] != "RunDelta-0.1.0"
                    or member.size > 500_000):
                raise ValueError("unsafe_archive_entry")
            name = "/".join(path.parts[1:])
            if name in files:
                raise ValueError("duplicate_archive_entry")
            files[name] = handle.extractfile(member).read()
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--history", action="store_true")
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--private-prefix", action="append", default=[])
    args = parser.parse_args()
    if not args.root and not args.archive:
        parser.error("specify a root or archive")
    findings = Counter()
    inspected = Counter()

    def add(scope, rows):
        for name, rules in rows.items():
            for rule, count in rules.items():
                findings[(scope, name, rule)] += count

    if args.root:
        files = worktree_files(args.root)
        add("worktree", inspect_files(files, WORKTREE, args.private_prefix))
        inspected["worktree_files"] = len(files)
    if args.history:
        if not args.root:
            parser.error("history requires a root")
        for files, metadata in history_files(args.root):
            add("history", inspect_files(files, WORKTREE, args.private_prefix))
            counts = privacy("commit-metadata", metadata, args.private_prefix)
            if counts:
                add("history", {"commit-metadata": counts})
            inspected["commits"] += 1
        if not inspected["commits"]:
            raise ValueError("no_history")
        if git(args.root, "remote").strip():
            raise ValueError("remote_present")
    if args.archive:
        files = archive_files(args.archive)
        add("archive", inspect_files(files, ARCHIVE, args.private_prefix))
        inspected["archive_files"] = len(files)
        vcs = json.loads(files[".cargo_vcs_info.json"])
        if vcs["path_in_vcs"] or vcs["git"].get("dirty"):
            raise ValueError("unclean_archive_origin")
        if args.root:
            if vcs["git"]["sha1"] != git(args.root, "rev-parse", "HEAD").decode().strip():
                raise ValueError("archive_origin_mismatch")
            for name in PAYLOAD - {"Cargo.toml"}:
                if files[name] != (args.root / name).read_bytes():
                    add("archive", {name: Counter({"source_mismatch": 1})})
            if files["Cargo.toml.orig"] != (args.root / "Cargo.toml").read_bytes():
                raise ValueError("manifest_source_mismatch")
    print(json.dumps({"passed": not findings, "checked": inspected,
                      "findings": [{"scope": scope, "file": name, "rule": rule, "count": n}
                                   for (scope, name, rule), n in sorted(findings.items())]}, sort_keys=True))
    return bool(findings)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError, tarfile.TarError):
        print(json.dumps({"passed": False, "rule": "validation_error", "count": 1}))
        raise SystemExit(1)
