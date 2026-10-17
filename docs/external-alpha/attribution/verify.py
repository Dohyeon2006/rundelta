#!/usr/bin/env python3
"""Verify the immutable alpha evidence and additive attribution independently.
The old validator runs unmodified against its exact original 26-file scope.
Its rejection of the expanded tree is also executed, not hidden or patched.
"""
from pathlib import Path
import ast
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ALPHA = HERE.parent
original = json.loads((HERE / 'original-materials.json').read_text())
assert len(original) == 26
new = {'README.md', 'RESULTS.json', 'original-materials.json', 'probe.c', 'probe.py', 'verify.py'}
allowed = set(original) | {'attribution/' + f for f in new}
actual = {p.relative_to(ALPHA).as_posix() for p in ALPHA.rglob('*') if p.is_file()}
assert actual == allowed, (actual - allowed, allowed - actual)
for name, digest in original.items():
    assert hashlib.sha256((ALPHA / name).read_bytes()).hexdigest() == digest, name
for name in sorted(actual):
    path = ALPHA / name
    assert not path.is_symlink()
    assert path.stat().st_size < 80_000, name
    text = path.read_text()
    def strings(value):
        if isinstance(value, str):
            yield value
        elif isinstance(value, dict):
            for key, val in value.items():
                yield key
                yield from strings(val)
        elif isinstance(value, list):
            for val in value:
                yield from strings(val)
    views = [text]
    if path.suffix == '.json':
        views += list(strings(json.loads(text)))
    for view in views:
        decoded = re.sub(r'(?:\\x[0-9a-fA-F]{2})+', lambda m: bytes.fromhex(m[0].replace('\\x', '')).decode('utf-8', 'replace'), view)
        assert not re.search(r'/(?:home|tmp|Users|root)/', decoded), name
        assert not re.search(r'(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]+)', decoded), name
    if path.suffix == '.py':
        ast.parse(text)
with tempfile.TemporaryDirectory(prefix='rundelta-original-alpha-verification-') as folder:
    snapshot = Path(folder)
    for name in original:
        target = snapshot / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ALPHA / name, target)
    check = subprocess.run([sys.executable, '-B', str(snapshot / 'verify.py')], text=True, capture_output=True)
    assert check.returncode == 0, check.stderr
    print('Original 26-file snapshot:', check.stdout.strip())
old_full = subprocess.run([sys.executable, '-B', str(ALPHA / 'verify.py')], text=True, capture_output=True)
assert old_full.returncode == 1 and 'AssertionError:' in old_full.stderr
print('Old validator against expanded tree: exit 1, fixed-inventory rejection retained (not reported as pass)')
results = json.loads((HERE / 'RESULTS.json').read_text())
assert sum(g['runs'] for g in results['groups']) == 132
assert results['nested_evidence']['matched_unknown_tids'] == results['nested_evidence']['matched_get_syscall_info_esrch'] == 9
assert results['original']['same_tid_resumed_count'] == 0
assert results['original']['parser_replay']['all_events_equal']
assert results['original']['parser_replay']['all_warnings_equal']
assert results['public_script_replay']['exit_code'] == 0
assert results['public_script_replay']['runs'] == 132
assert results['conclusions']['spades_becomes_complete_if_only_unknown_resolved'] is False
print('PASS: original hashes unchanged; strict combined inventory; all public text/decoded strings redacted; probe results and parser replay consistent')
