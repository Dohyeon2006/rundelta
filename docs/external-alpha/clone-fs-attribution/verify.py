#!/usr/bin/env python3
"""Verify the additive CLONE_FS evidence without changing either old validator.
The 26/32-file fixed inventories retain their original scopes and assertions.
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
old = json.loads((HERE / 'original-materials.json').read_text())
assert len(old) == 32
new = {'README.md', 'PLAN.md', 'probe.c', 'collect.py', 'RESULTS.json', 'EVIDENCE.json',
       'original-materials.json', 'verify.py'}
allowed = set(old) | {'clone-fs-attribution/' + name for name in new}
actual = {p.relative_to(ALPHA).as_posix() for p in ALPHA.rglob('*') if p.is_file()}
assert actual == allowed, (actual - allowed, allowed - actual)
for name, digest in old.items():
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
        decoded = re.sub(r'(?:\\x[0-9a-fA-F]{2})+',
                         lambda m: bytes.fromhex(m[0].replace('\\x', '')).decode('utf-8', 'replace'), view)
        assert not re.search(r'/(?:home|tmp|Users|root)/', decoded), name
        assert not re.search(r'(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]+)', decoded), name
    if path.suffix == '.py':
        ast.parse(text)
with tempfile.TemporaryDirectory(prefix='rundelta-alpha-32-verification-') as folder:
    snapshot = Path(folder)
    for name in old:
        target = snapshot / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ALPHA / name, target)
    result = subprocess.run([sys.executable, '-B', str(snapshot / 'attribution/verify.py')], text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    print('Original 32-file snapshot (including nested original 26-file check): PASS')
for name in ['verify.py', 'attribution/verify.py']:
    result = subprocess.run([sys.executable, '-B', str(ALPHA / name)], text=True, capture_output=True)
    assert result.returncode == 1 and 'AssertionError:' in result.stderr
    print(name + ': exit 1 against expanded tree, fixed-inventory rejection retained (not a pass)')
results = json.loads((HERE / 'RESULTS.json').read_text())
evidence = json.loads((HERE / 'EVIDENCE.json').read_text())
assert hashlib.sha256((HERE / 'probe.c').read_bytes()).hexdigest() == results['probe_sha256']
assert hashlib.sha256((HERE / 'PLAN.md').read_bytes()).hexdigest() == results['plan_sha256']
assert len(results['groups']) == 15 and sum(r['samples'] for r in results['groups']) == 45
assert results['public_script_replay']['exit_code'] == 0 and results['public_script_replay']['runs'] == 45
for group in results['groups']:
    assert group['record_exit_codes'] == group['target_exit_codes'] == group['collector_exit_codes'] == [0, 0, 0]
    assert group['storage'] == ['verified'] * 3
    assert group['marker_checks_passed'] and group['expected_open_path_observed']
    assert group['repeat_comparison']['observed_differences'] is False
    if group['clone_fs_raw'] == [True] * 3:
        assert group['completeness'] == ['partial'] * 3
        assert group['first_payload_event']['cwd_before'] is None
    assert group['first_payload_event']['path'].endswith('/' + group['expected_marker'] + '/payload.txt')
    assert group['first_payload_event']['context_uncertain'] is False
    for item in evidence[group['case']]['events']:
        event = item['event']
        if event['operation'] == 'access':
            assert event['path'].startswith('relative:') and event['context_uncertain']
        ev = event['evidence']
        assert ev['file'] == 'raw/trace.log' and 0 < ev['line'] <= ev['end_line']
        assert [line['line'] for line in item['decoded_redacted_raw_lines']] == list(range(ev['line'], ev['end_line'] + 1))
print('PASS: unchanged original 32 files, strict expanded inventory, decoded public-path scan, 90-run evidence consistency')
