#!/usr/bin/env python3
"""Run the five preregistered holdout cases; never tune product code here.
Linux, OpenSSL, Python, tar, Git, sha256sum, working strace required. No network.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('RUNDELTA_BIN', ROOT/'target/debug/rundelta')).resolve()
WORK = Path(tempfile.mkdtemp(prefix='holdout-', dir=ROOT/'target'))
STORE = WORK/'records'
ENV = os.environ.copy()
ENV.update(LC_ALL='C', PYTHONDONTWRITEBYTECODE='1', RUNDELTA_STRACE=os.environ.get('RUNDELTA_STRACE', str(ROOT/'target/tools/strace-6.16/src/strace')))
for k in list(ENV):
    if k.startswith(('GIT_', 'PYTHON')) and k != 'PYTHONDONTWRITEBYTECODE':
        ENV.pop(k)
ENV.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
LOG = []
RESULTS = []
FROZEN = {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in [*ROOT.joinpath('src').rglob('*.rs'), ROOT/'Cargo.toml', ROOT/'Cargo.lock', ROOT/'docs/HOLDOUT_PLAN.md', Path(__file__).resolve()]}
MANIFEST = dict(registered_at=datetime.now(timezone.utc).isoformat(), workdir=str(WORK), sha256=FROZEN)
(WORK/'manifest.json').write_text(json.dumps(MANIFEST, indent=2))

def command(argv, cwd=None, check=True):
    p = subprocess.run(list(map(str, argv)), cwd=cwd or ROOT, env=ENV, capture_output=True, text=True, timeout=90)
    LOG.append(dict(argv=list(map(str, argv)), cwd=str(cwd or ROOT), returncode=p.returncode, stdout=p.stdout, stderr=p.stderr))
    if check:
        assert p.returncode == 0, LOG[-1]
    return p

def rd(*args, cwd=None, check=True):
    return command([BIN, '--storage', STORE, *args], cwd=cwd, check=check)

def experiment(name, argv, cwd, prepare, mutate, restore, clue):
    print('START', name, flush=True)
    runs = {}
    for phase in ['good', 'repeat', 'bad']:
        prepare()
        if phase == 'bad':
            mutate()
        try:
            p = rd('record', name+'-'+phase, '--', *argv, cwd=cwd, check=False)
            (WORK/f'{name}-{phase}.stdout').write_text(p.stdout)
            (WORK/f'{name}-{phase}.stderr').write_text(p.stderr)
            assert (p.returncode == 0 if phase != 'bad' else p.returncode not in (0, 125)), (name, phase, p.returncode, p.stderr)
            r = json.loads(rd('show', name+'-'+phase, '--json').stdout)
            assert r['storage_integrity']['status'] == 'verified'
            assert r['metadata']['state'] == 'completed'
            runs[phase] = {k:r['metadata'][k] for k in ['id','exit_code','signal','warnings','completeness','parsed_events']}
        finally:
            if phase == 'bad':
                restore()
    reports = {}
    for label, right in [('control', 'repeat'), ('comparison', 'bad')]:
        p = rd('diff', name+'-good', name+'-'+right, '--json', check=False)
        assert p.returncode in (0, 1, 3), p.stdout+p.stderr
        data = json.loads(p.stdout)
        text = rd('diff', name+'-good', name+'-'+right, check=False)
        assert text.returncode == p.returncode == data['exit_code']
        (WORK/f'{name}-{label}.json').write_text(p.stdout)
        (WORK/f'{name}-{label}.txt').write_text(text.stdout)
        reports[label] = data
    d = reports['comparison']
    ranks = [i+1 for i,c in enumerate(d['changes']) if clue(c)]
    row = dict(case=name, command=list(map(str,argv)), cwd=str(cwd), runs=runs,
               key_clue_ranks=ranks, first_key_clue_rank=min(ranks) if ranks else None,
               top5_hit=bool(ranks and min(ranks)<=5), total_changes=len(d['changes']),
               control_changes=len(reports['control']['changes']), reliability=d['reliability'],
               diff_exit_code=d['exit_code'], top5=d['changes'][:5])
    RESULTS.append(row)
    (WORK/'results.json').write_text(json.dumps(RESULTS,indent=2))
    print(f'FINISH {name}: top5={row["top5_hit"]}, rank={row["first_key_clue_rank"]}, changes={row["total_changes"]}, control={row["control_changes"]}, diff={row["diff_exit_code"]}',flush=True)

try:
    for tool, flag in [('openssl','version'),('python3','--version'),('tar','--version'),('git','--version'),('sha256sum','--version')]:
        command([tool,flag])
    # 1. Actual certificate chain validation through an OpenSSL hashed trust directory.
    p=WORK/'openssl-trust';p.mkdir(); trust=p/'trust';trust.mkdir()
    command(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-keyout','ca.key','-out','ca.pem','-days','1','-subj','/CN=RunDelta Holdout CA'],cwd=p)
    command(['openssl','req','-newkey','rsa:2048','-nodes','-keyout','leaf.key','-out','leaf.csr','-subj','/CN=build.example.invalid'],cwd=p)
    command(['openssl','x509','-req','-in','leaf.csr','-CA','ca.pem','-CAkey','ca.key','-CAcreateserial','-out','leaf.pem','-days','1'],cwd=p)
    h=command(['openssl','x509','-in','ca.pem','-noout','-hash'],cwd=p).stdout.strip()
    link=trust/(h+'.0');link.symlink_to('../ca.pem')
    experiment('openssl-trust',['openssl','verify','-no-CAfile','-no-CAstore','-CApath','trust','leaf.pem'],p,
               lambda:None,lambda:link.unlink(),lambda:link.symlink_to('../ca.pem'),
               lambda c: '/trust/'+h+'.0' in c['subject'])
    # 2. A working stdlib serialization command shadowed by a new local module.
    p=WORK/'python-shadow';p.mkdir();shadow=p/'json.py'
    experiment('python-shadow',['python3','-c','import json; print(json.dumps({"build": "release"}))'],p,
               lambda:None,lambda:shadow.write_text('# Local module accidentally shadows the standard library.\n'),lambda:shadow.unlink(),
               lambda c: str(shadow) in c['subject'] and 'success' in c['right'])
    # 3. Extract real project material, then remove write permission on the destination.
    p=WORK/'tar-permission';p.mkdir();out=p/'output';out.mkdir();archive=p/'source.tar'
    command(['tar','-cf',archive,'-C',ROOT,'README.md'])
    def clean_output():
        (out/'README.md').unlink(missing_ok=True)
    experiment('tar-permission',['tar','-xf','source.tar','-C','output'],p,clean_output,
               lambda:out.chmod(0o500),lambda:out.chmod(0o700),
               lambda c: '/output/README.md' in c['subject'] and c['left']==['success'] and c['right']==['EACCES'])
    # 4. A real shared Git clone depends on the donor's object storage.
    p=WORK/'git-alternates';p.mkdir();donor=p/'donor';clone=p/'clone'
    command(['git','init','-q',donor]);shutil.copyfile(ROOT/'README.md',donor/'README.md')
    command(['git','add','README.md'],cwd=donor)
    command(['git','-c','user.name=RunDelta Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture input'],cwd=donor)
    command(['git','clone','-q','--shared',donor,clone])
    objects=donor/'.git/objects';backup=donor/'.git/objects.saved'
    experiment('git-alternates',['git','show','HEAD:README.md'],clone,lambda:None,
               lambda:objects.rename(backup),lambda:backup.rename(objects),
               lambda c: str(objects)+'/' in c['subject'] and 'success' in c['left'] and 'success' not in c['right'])
    # 5. Real source-archive checksum, with undetectable-by-path byte drift.
    p=WORK/'checksum-content';p.mkdir();payload=p/'source.tar';shutil.copyfile(archive,payload)
    digest=command(['sha256sum','source.tar'],cwd=p).stdout
    (p/'SHA256SUMS').write_text(digest)
    original=payload.read_bytes()
    experiment('checksum-content',['sha256sum','--check','SHA256SUMS'],p,lambda:None,
               lambda:payload.write_bytes(bytes([original[0]^1])+original[1:]),lambda:payload.write_bytes(original),
               lambda c: False) # Preregistered: no content snapshot, no path-only proxy hit.
finally:
    (WORK/'commands.json').write_text(json.dumps(LOG,indent=2))
    (WORK/'results.json').write_text(json.dumps(RESULTS,indent=2))
    assert all(hashlib.sha256((ROOT/p).read_bytes()).hexdigest()==v for p,v in FROZEN.items()), 'frozen product/plan changed'
    print('ARTIFACTS:',WORK,flush=True)
