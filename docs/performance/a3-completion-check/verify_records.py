#!/usr/bin/env python3
"""Optional post-measurement read audit, NOT a completion-check benchmark."""
import argparse
from collections import Counter
import json
from pathlib import Path
import subprocess
p=argparse.ArgumentParser();p.add_argument('artifacts',type=Path);p.add_argument('--binary',type=Path,required=True)
a=p.parse_args();result=json.loads((a.artifacts/'results.json').read_text());checks=[]
for group in result['groups']:
    e,processes=group['events'],group['processes']
    store=a.artifacts/f'warm-{e}-{processes}'/'records'
    cli=subprocess.run([str(a.binary.resolve()),'--storage',str(store),'show','synthetic','--json'],
                       capture_output=True,text=True,timeout=30,check=True)
    run=json.loads(cli.stdout);events=run['events'];seen=Counter();first_exit={}
    for i,event in enumerate(events,1):
        seen[event['pid']]+=1
        if event['child_pid'] is not None:seen[event['child_pid']]+=1
        if event['kind']=='exit':first_exit.setdefault(event['pid'],i)
    predicted=sum(count*first_exit.get(pid,len(events)) for pid,count in seen.items())
    assert len(events)==e and len(seen)==processes and len(first_exit)==processes
    assert predicted==group['predicted_any_predicate_visits']
    assert run['storage_integrity']['status']=='verified'
    assert run['metadata']['backend'].startswith('A3 synthetic fixture producer')
    checks.append(dict(events=len(events),processes=len(seen),exited_processes=len(first_exit),
                       modeled_predicate_visits_from_loaded_events=predicted,storage='verified',
                       source='actual product show of warmup record; audit outside timed samples'))
print(json.dumps(checks,indent=2))
