#!/usr/bin/env python3
"""Optional minimum-work GDB calibration; not a replacement for the E x P matrix."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
from measure import fixture, stats, HERE

p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);args=p.parse_args()
w=Path(tempfile.mkdtemp(prefix='rundelta-a3-calibration-'))
fixture(w/'trace',2,1)
samples=[]
for i in range(5):
    env=os.environ|{'RUNDELTA_STRACE':str(HERE/'synthetic_backend.py'),
                    'A3_TRACE_INPUT':str(w/'trace'),'A3_PROBE_OUTPUT':str(w/f'{i}.json')}
    cmd=['gdb','-q','-batch','-ex','set debuginfod enabled off','-ex','set pagination off',
         '-ex','set disable-randomization off','-ex','source '+str(HERE/'probe_gdb.py'),'-ex','run','--args',
         str(args.binary.resolve()),'--storage',str(w/f'store-{i}'),'record','calibration','--','synthetic-no-target']
    run=subprocess.run(cmd,env=env,cwd='/',stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,timeout=30)
    assert run.returncode==0,'debugger failed; inspect local environment'
    data=json.loads((w/f'{i}.json').read_text())
    assert data['hits']=={'start':1,'end':1,'final':1} and data['exit_code']==0
    samples.append(data)
print(json.dumps(dict(events=2,processes=1,predicted_any_predicate_visits=4,samples=samples,
    completion_wall_ms=stats([d['completion_wall_ms'] for d in samples]),
    note='Additional post-matrix minimum-work calibration; not subtracted from primary results; not a measured pure debugger overhead'),indent=2))
