#!/usr/bin/env python3
"""Read-only checks of public summaries; does not reclassify failed experiments."""
from pathlib import Path
import ast
import hashlib
import json
import re

ROOT = Path(__file__).resolve().parent
CASES = ['pybind11-3445', 'spades-1339', 'meson-16136']
ROOT_FILES = {'README.md', 'REPRODUCE.md', 'provenance.json', 'preservation.json',
              'CHECKS.md', 'REPLAY.json', 'reproduce.py', 'verify.py'}
CASE_FILES = {'CASE.md', 'RESULTS.md', 'summary.json', 'evidence-excerpts.json', 'preregistration.sha256'}
EXTRA = {'spades-1339/RETRY.md', 'spades-1339/RETRY-EXACT.md', 'spades-1339/attempts.json'}
allowed = ROOT_FILES | {f'{case}/{file}' for case in CASES for file in CASE_FILES} | EXTRA
actual = {p.relative_to(ROOT).as_posix() for p in ROOT.rglob('*') if p.is_file()}
assert actual == allowed, (actual - allowed, allowed - actual)
for relative in sorted(actual):
    path = ROOT / relative
    assert not path.is_symlink()
    assert path.stat().st_size < 80_000, relative
    text = path.read_text()
    views = [text]
    if path.suffix == '.json':
        def strings(value):
            if isinstance(value, str):
                yield value
            elif isinstance(value, dict):
                for key, item in value.items():
                    yield key
                    yield from strings(item)
            elif isinstance(value, list):
                for item in value:
                    yield from strings(item)
        views += list(strings(json.loads(text)))
    for view in views:
        decoded = re.sub(r'(?:\\x[0-9a-fA-F]{2})+',
                         lambda m: bytes.fromhex(m[0].replace('\\x', '')).decode('utf-8', 'replace'), view)
        assert not re.search(r'/(?:home|tmp|Users|root)/', decoded), relative
        assert not re.search(r'(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]+)', decoded), relative
    if path.suffix == '.py':
        ast.parse(text)
for case in CASES:
    directory = ROOT / case
    expected = (directory / 'preregistration.sha256').read_text().split()[0]
    assert hashlib.sha256((directory / 'CASE.md').read_bytes()).hexdigest() == expected
    report = json.loads((directory / 'summary.json').read_text())
    assert sum(report['categories'].values()) == report['total_changes']
    assert [entry['rank'] for entry in report['top_five']] == [1, 2, 3, 4, 5]
    assert report['observed_differences'] is True and report['errors'] == []
    expected_capture, expected_exit = ('complete', 1) if case == 'pybind11-3445' else ('incomplete', 3)
    assert report['exit_code'] == expected_exit
    for side in ['left', 'right']:
        assert report['reliability'][side] == {'capture': expected_capture, 'storage': 'verified'}
    for entry in json.loads((directory / 'evidence-excerpts.json').read_text()):
        assert entry['evidence']['file'] == 'raw/trace.log'
        assert 0 < entry['evidence']['line'] <= entry['evidence']['end_line']
        assert 'decoded_redacted_excerpt' in entry
pybind = json.loads((ROOT / CASES[0] / 'summary.json').read_text())
assert pybind['top_five'][1]['subject'] == 'CMAKE_PREFIX_PATH'
spades = json.loads((ROOT / 'spades-1339/attempts.json').read_text())
assert [a['record_exit_codes'] for a in spades] == [[127, 127], [0, 0], [0, 4]]
replay = json.loads((ROOT / 'REPLAY.json').read_text())
assert replay['script_exit_code'] == 0
assert replay['strict_hits'] == {'pybind': True, 'meson': False, 'spades': False}
print('PASS: public file allowlist, preregistration hashes, summary/reliability consistency, decoded path/token scan, source syntax, replay outcomes')
