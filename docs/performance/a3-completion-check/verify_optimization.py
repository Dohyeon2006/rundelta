#!/usr/bin/env python3
"""Recompute A3 optimization statistics, compare fixture hashes, retain failures."""
import hashlib
import json
from pathlib import Path
import statistics

H = Path(__file__).resolve().parent
baseline = json.loads((H/'RESULTS.json').read_text())
expected = {(g['events'],g['processes']):g['trace_sha256'] for g in baseline['groups']}
for name in ['DEV_AFTER','RELEASE_BEFORE','RELEASE_AFTER','PILOT_DEV_AFTER']:
    r = json.loads((H/'optimization'/f'{name}.json').read_text())
    assert r['failures']==[] and r['tracked_files_unchanged']
    assert len(r['samples'])==27 and len(r['groups'])==9 and r['warmups']==9
    if name != 'PILOT_DEV_AFTER':
        assert r['interval']['adapter_sha256']==hashlib.sha256((H/'compare.py').read_bytes()).hexdigest()
    for g in r['groups']:
        assert expected[g['events'],g['processes']]==g['trace_sha256']
        rows = [s for s in r['samples'] if (s['events'],s['processes'])==(g['events'],g['processes'])]
        assert len(rows)==3 and {s['repeat'] for s in rows}=={0,1,2}
        for row in rows:
            assert row['hits']=={'start':1,'end':1,'final':1} and row['exit_code']==0
            assert all(row[k+'_frame']=='rundelta::capture::record' for k in ['start','end','final'])
        for key in ['completion_wall_ms','record_wall_ms','show_wall_ms','start_memory_kib','end_memory_kib','final_memory_kib','record_time','show_time']:
            field = 'max_rss_kib' if key.endswith('time') else 'VmHWM'
            vals = [s[key] if key.endswith('_ms') else s[key][field] for s in rows]
            summary = key+'_max_rss_kib' if key.endswith('time') else key
            assert g[summary]==dict(min=min(vals),median=statistics.median(vals),max=max(vals),sample_stdev=statistics.stdev(vals))
f = json.loads((H/'optimization/PROBE_FAILURES.json').read_text())
assert len(f)==2 and all(x['result']['failures'] and x['probes'] for x in f)
checks = json.loads((H/'optimization/RECORD_CHECKS.json').read_text())
assert len(checks)==3 and all(len(rows)==9 and all(row['storage']=='verified' for row in rows) for rows in checks.values())
s = json.loads((H/'optimization/SEMANTICS.json').read_text())
assert s['count']==20 and s['before_sha256']==s['after_sha256']
assert len(s['profiles'])==4 and set(s['profiles'].values())=={s['before_sha256']}
field=json.loads((H/'optimization/FIELD_REPLAY.json').read_text())
assert len(field)==15 and all(row['identical'] for row in field)
status=json.loads((H/'optimization/TEST_RESULTS.json').read_text())
assert {r['check'] for r in status if r['exit_code']!=0}=={'field','field-old-binary-on-historical','field-fresh-audit','field-fresh-old-binary-audit'}
assert s['test_sha256']==hashlib.sha256((H.parents[2]/'tests/completion_equivalence.py').read_bytes()).hexdigest()
closure=json.loads((H/'optimization/AUDIT_CLOSURE.json').read_text())
assert all(row['returncode']==1 for row in closure['before'])
assert all(row['returncode']==0 for row in closure['final_checks'])
assert closure['fresh_capture_exit_code']==closure['fresh_audit_exit_code']==closure['mutation_audit_exit_code']==0
assert closure['mutation_records']==15 and closure['rejected_mutations']==222 and closure['record_files_unchanged']
expected_counts={'python-venv':(0,0,0),'pkg-config':(1,1,1),'git-identity':(18,18,4),'gcc-header':(0,0,0),'cargo-linker':(1,1,1)}
assert len(closure['cases'])==5 and {c['case'] for c in closure['cases']}==set(expected_counts)
for case in closure['cases']:
    assert 1<=case['first_useful_rank']<=5
    for phase,count in zip(['good','repeat','bad'],expected_counts[case['case']]):
        audit=case['warning_audit'][phase]
        assert audit['expected']==audit['actual']
        assert sum(row['count'] for row in audit['expected'])==len(audit['warnings'])==count
        assert audit['capture']==('partial' if count else 'complete_for_supported_events')
        assert audit['storage']=='verified'
    incomplete=bool(expected_counts[case['case']][0])
    assert case['diff_exit_code']==(3 if incomplete else 1)
    assert all(q==dict(capture='incomplete' if incomplete else 'complete',storage='verified') for q in case['reliability'].values())
assert closure['historical_unexpected_warnings']==dict(expected_returncode=1,actual_returncode=1,passed=True)
for path in (H/'optimization').glob('*.json'):
    assert not any(x in path.read_text() for x in ['/home/','/tmp/','/target/']), path.name
print('PASS: 81 final samples + 27 retained pilot samples; same input hashes; statistics/frames/27 reads/20 contracts; two rejected probe attempts retained; final audit closure verified; public path scan')
