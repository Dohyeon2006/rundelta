#!/usr/bin/env python3
"""Audit existing real field-case records without rewriting them.
Usage: python3 tests/field_report_audit.py target/field-cases-<id>
First run examples/field_cases.py with the binary being evaluated.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
from collections import Counter

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('RUNDELTA_BIN', ROOT / 'target/debug/rundelta')).resolve()
WORK = Path(sys.argv[1]).resolve()
results = json.loads((WORK / 'results.json').read_text())
assert len(results) == 5
summary = []
for case in results:
    name = case['case']
    reports = {}
    for suffix, right in [('comparison', 'bad'), ('control', 'repeat')]:
        cmd = [str(BIN), '--storage', str(WORK / 'records'), 'diff', name + '-good', name + '-' + right]
        p = subprocess.run(cmd + ['--json'], capture_output=True, text=True, timeout=60)
        data = json.loads(p.stdout)
        assert p.returncode == data['exit_code']
        assert data['schema_version'] == 4
        t = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        assert t.returncode == p.returncode
        assert data['observed_differences'] == bool(data['changes'])
        if p.returncode == 3:
            assert 'RELIABILITY NOT ESTABLISHED' in t.stdout
        else:
            assert p.returncode == int(bool(data['changes']))
        assert all(q['storage'] == 'verified' for q in data['reliability'].values())
        groups = Counter(c['detail_group'] for i, c in enumerate(data['changes']) if i >= 5 and c['detail_group'])
        assert f'Total observed changes: {len(data["changes"])}' in t.stdout
        for group, count in groups.items():
            assert group in t.stdout and f'{count} changes folded' in t.stdout
        reports[suffix] = (data, groups)
    report, _ = reports['comparison']
    control, groups = reports['control']
    clues = [i + 1 for i, c in enumerate(report['changes']) if any(f in c['subject'] for f in case['clue_fragments'])]
    assert clues and min(clues) <= 5, (name, clues)
    assert not any(c['category'] in ['Executable changes', 'Execution attempt changes'] for c in control['changes']), name
    for phase in ['good', 'repeat', 'bad']:
        p = subprocess.run([str(BIN), '--storage', str(WORK / 'records'), 'show', name + '-' + phase, '--json'], capture_output=True, text=True, timeout=60)
        assert p.returncode == 0
        run = json.loads(p.stdout)
        warnings = run['metadata']['warnings']
        # Preserve the conservative CLONE_FS warning, not a blanket permission to ignore warnings.
        if name == 'cargo-linker':
            assert warnings == ['shared filesystem state or root/namespace changes observed; cwd fallback disabled'], warnings
        else:
            assert warnings == [], (name, phase, warnings)
    if name == 'gcc-header':
        assert report['changes'][1]['subject'].endswith('/src/defs.h (openat)')
        assert report['changes'][1]['left'] == ['success']
        assert report['changes'][1]['right'] == ['ENOENT']
    if name == 'cargo-linker':
        c = report['changes'][1]
        assert c['category'] == 'Execution attempt changes'
        assert c['subject'].endswith('/missing-linker (execve)')
        assert c['right'] == ['ENOENT'] and c['right_evidence']
    item = dict(case=name, first_useful_rank=min(clues), total_changes=len(report['changes']),
                control_changes=len(control['changes']), folded_control=sum(groups.values()),
                control_display_entries=len(control['changes'])-sum(groups.values())+len(groups),
                reliability=report['reliability'], diff_exit_code=report['exit_code'],
                top5=report['changes'][:5], runs=case['runs'])
    summary.append(item)
    print(f'PASS {name}: clue={min(clues)}, control={item["control_changes"]}, folded={item["folded_control"]}, display={item["control_display_entries"]}, diff={report["exit_code"]}')
(WORK / 'repair-audit.json').write_text(json.dumps(summary, indent=2))
print('PASS: five cases, no execution differences in normal controls, warnings and reliability retained')
