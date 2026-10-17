#!/usr/bin/env python3
"""Recompute public statistics and fingerprints without opening private records."""
import hashlib
import json
from pathlib import Path
import statistics
H=Path(__file__).resolve().parent
r=json.loads((H/'RESULTS.json').read_text())
assert len(r['samples'])==27 and len(r['groups'])==9 and r['failures']==[]
assert r['tracked_files_unchanged']
for name,expected in r['script_sha256'].items():
    assert hashlib.sha256((H/name).read_bytes()).hexdigest()==expected,name
assert hashlib.sha256((H/'PLAN.md').read_bytes()).hexdigest()==r['plan_sha256']
for g in r['groups']:
    rows=[s for s in r['samples'] if (s['events'],s['processes'])==(g['events'],g['processes'])]
    assert len(rows)==3 and {s['repeat'] for s in rows}=={0,1,2}
    assert g['pid_candidates']==g['events']+g['processes']-1
    for s in rows:
        assert s['hits']=={'start':1,'end':1,'final':1} and s['exit_code']==0
    for key in ['completion_wall_ms','record_wall_ms','show_wall_ms']:
        vals=[s[key] for s in rows]
        assert g[key]==dict(min=min(vals),median=statistics.median(vals),max=max(vals),sample_stdev=statistics.stdev(vals))
    for key in ['start_memory_kib','end_memory_kib','final_memory_kib','record_time','show_time']:
        field='max_rss_kib' if key.endswith('time') else 'VmHWM'
        vals=[s[key][field] for s in rows]
        summary=key+'_max_rss_kib' if key.endswith('time') else key
        assert g[summary]==dict(min=min(vals),median=statistics.median(vals),max=max(vals),sample_stdev=statistics.stdev(vals))
for name in ['RESULTS.json','CALIBRATION.json']:
    text=(H/name).read_text()
    for forbidden in ['/home/','/tmp/','/target/']:
        assert forbidden not in text,(name,forbidden)
c=json.loads((H/'CALIBRATION.json').read_text())
assert len(c['samples'])==5 and c['events']==2
print('PASS: nine groups, 27 samples, statistics/hashes, five calibration samples and public path scan')
