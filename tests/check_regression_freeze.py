#!/usr/bin/env python3
"""Verify immutable v1 evidence and the explicitly authorized v2 warning contract.
The original audit is archived byte-for-byte. Only the exact warning-gate/report
patch below may differ; ranking, clues and existing reliability assertions remain.
"""
import hashlib
import json
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
manifest = json.loads((ROOT/'docs/REGRESSION_SET.json').read_text())
archive = 'tests/fixtures/field_report_audit_v1.py'
for name, expected in manifest['sha256'].items():
    source = archive if name == 'tests/field_report_audit.py' else name
    actual = hashlib.sha256((ROOT/source).read_bytes()).hexdigest()
    assert actual == expected, f'frozen v1 artifact changed: {source}'

old_gate = '''        warnings = run['metadata']['warnings']
        # Preserve the conservative CLONE_FS warning, not a blanket permission to ignore warnings.
        if name == 'cargo-linker':
            assert warnings == ['shared filesystem state or root/namespace changes observed; cwd fallback disabled'], warnings
        else:
            assert warnings == [], (name, phase, warnings)
'''
new_gate = '''        raw = (WORK / 'records' / 'runs' / run['metadata']['id'] / 'raw' / 'trace.log').read_text().splitlines()
        warning_audit[phase] = audit_warnings(name, phase, run, raw)
        print('WARNINGS ' + json.dumps(dict(case=name, phase=phase, **warning_audit[phase]), sort_keys=True))
    for suffix, right_phase in [('comparison', 'bad'), ('control', 'repeat')]:
        data = reports[suffix][0]
        for side, phase in [('left', 'good'), ('right', right_phase)]:
            expected_capture = 'incomplete' if warning_audit[phase]['expected'] else 'complete'
            assert data['reliability'][side] == dict(capture=expected_capture, storage='verified')
        if warning_audit['good']['expected'] or warning_audit[right_phase]['expected']:
            assert data['exit_code'] == 3
'''
patches = [
    ('from collections import Counter', 'from collections import Counter\nfrom field_warning_contract import audit_warnings'),
    ("    for phase in ['good', 'repeat', 'bad']:", "    warning_audit = {}\n    for phase in ['good', 'repeat', 'bad']:"),
    (old_gate,new_gate),
    ("                top5=report['changes'][:5], runs=case['runs'])", "                top5=report['changes'][:5], runs=case['runs'], warning_audit=warning_audit)")
]
expected = (ROOT/archive).read_text()
for old,new in patches:
    assert expected.count(old)==1, 'ambiguous audit upgrade'
    expected = expected.replace(old,new)
assert (ROOT/'tests/field_report_audit.py').read_text()==expected, 'audit changed outside the authorized warning contract patch'
contract = json.loads((ROOT/'tests/fixtures/field_audit_contract_v2.json').read_text())
assert contract['version']==2 and contract['v1_audit_archive']==archive
for name,expected in contract['sha256'].items():
    assert hashlib.sha256((ROOT/name).read_bytes()).hexdigest()==expected, f'v2 warning contract changed: {name}'
print('PASS: original v1 inputs/audit/results preserved; v2 changes limited to warning contract and explicit reliability assertions; v2 fingerprints verified')
