#!/usr/bin/env python3
"""Read-only mutation audit against all 15 real records; never rewrites evidence."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from field_warning_contract import audit_warnings

work = Path(sys.argv[1]).resolve()
root = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get('RUNDELTA_BIN',root/'target/debug/rundelta')).resolve()
files = sorted(p for p in (work/'records').rglob('*') if p.is_file())
before = {p:hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
checks = []
extra_warnings = ['raw/trace.log:1: unsupported or incomplete event: test()',
                  'cannot read trace: test failure', 'collector status does not confirm target completion',
                  'process completion not observed for PIDs: 999', 'unrecognized target termination signal',
                  'storage checksum mismatch', 'raw/trace.log:1: execution arguments incomplete',
                  'raw/trace.log:1: path context unresolved; extra CWD warning',
                  'shared filesystem state or root/namespace changes observed; extra source']
for case in json.loads((work/'results.json').read_text()):
    for phase in ['good','repeat','bad']:
        name = case['case']
        command = [str(binary),'--storage',str(work/'records'),'show',name+'-'+phase,'--json']
        run = json.loads(subprocess.check_output(command,text=True))
        raw = (work/'records/runs'/run['metadata']['id']/'raw/trace.log').read_text().splitlines()
        result = audit_warnings(name,phase,run,raw)
        count = 0
        mutations = []
        for warning in extra_warnings:
            modified = copy.deepcopy(run)
            modified['metadata']['warnings'].append(warning)
            mutations.append(modified)
        if run['metadata']['warnings']:
            for warnings in [run['metadata']['warnings'][1:], ['unknown category', *run['metadata']['warnings'][1:]], run['metadata']['warnings']*2]:
                modified = copy.deepcopy(run);modified['metadata']['warnings']=warnings;mutations.append(modified)
        for field,value in [('state','capture_failed'),('completeness','pending'),('collector_signal',15)]:
            modified=copy.deepcopy(run);modified['metadata'][field]=value;mutations.append(modified)
        modified=copy.deepcopy(run);modified['storage_integrity']['status']='corrupt';mutations.append(modified)
        for modified in mutations:
            try:
                audit_warnings(name,phase,modified,raw)
            except AssertionError:
                count += 1
            else:
                raise AssertionError((name,phase,'mutation was accepted'))
        checks.append(dict(case=name,phase=phase,expected=result['expected'],actual=result['actual'],rejected_mutations=count))
assert len(checks)==15
assert all(hashlib.sha256(p.read_bytes()).hexdigest()==digest for p,digest in before.items())
print(json.dumps(dict(records=15,rejected_mutations=sum(x['rejected_mutations'] for x in checks),
                     record_files_unchanged=True,checks=checks),indent=2))
