#!/usr/bin/env python3
"""Real captures for three independent workloads; all outputs live outside the repo.
Set RUNDELTA_BIN, RUNDELTA_STRACE, CMAKE_BIN and PIP_PYTHON. The last interpreter
must already contain pip; installing tooling is deliberately separate.
"""
import base64
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import zipfile

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
RD=Path(os.environ.get('RUNDELTA_BIN',ROOT/'target/debug/rundelta')).resolve()
CMAKE=os.environ.get('CMAKE_BIN') or shutil.which('cmake')
PYTHON=os.environ.get('PIP_PYTHON')
if not CMAKE or not PYTHON or os.geteuid()==0:
    raise SystemExit('CMAKE_BIN and PIP_PYTHON required; use a non-root user.')
W=Path(tempfile.mkdtemp(prefix='rundelta-fold-captures-'))
ENV=os.environ.copy()
for key in ['CPATH','CPLUS_INCLUDE_PATH','C_INCLUDE_PATH','CXXFLAGS','CFLAGS','RUSTFLAGS',
            'CARGO_ENCODED_RUSTFLAGS','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','PYTHONPATH','PYTHONHOME']:
    ENV.pop(key,None)
ENV.update(LC_ALL='C',PYTHONDONTWRITEBYTECODE='1',PIP_CONFIG_FILE=os.devnull,PIP_DISABLE_PIP_VERSION_CHECK='1',
           PIP_NO_CACHE_DIR='1',CARGO_NET_OFFLINE='true')
LOG=[];RESULTS=[]
frozen={str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in ROOT.joinpath('src').rglob('*.rs')}
manifest=dict(plan_sha256=hashlib.sha256((HERE/'EXPECTED_CLUES.md').read_bytes()).hexdigest(),source_sha256=frozen,
              binary_sha256=hashlib.sha256(RD.read_bytes()).hexdigest())
(W/'manifest.json').write_text(json.dumps(manifest,indent=2))
print('ARTIFACTS:',W,flush=True)

def run(args,cwd=W,check=True):
    p=subprocess.run(list(map(str,args)),cwd=cwd,env=ENV,capture_output=True,text=True,timeout=180)
    LOG.append(dict(argv=list(map(str,args)),cwd=str(cwd),returncode=p.returncode,stdout=p.stdout,stderr=p.stderr))
    (W/'commands.json').write_text(json.dumps(LOG,indent=2))
    if check: assert p.returncode==0,(args,p.returncode,p.stderr)
    return p

def experiment(name,cwd,cmd,clean,mutate,restore,clue):
    records={}
    for phase in ['good','repeat','bad']:
        clean()
        if phase=='bad':mutate()
        try:
            p=run([RD,'--storage',W/'records','record',name+'-'+phase,'--',*cmd],cwd,False)
            (W/f'{name}-{phase}.stdout').write_text(p.stdout)
            (W/f'{name}-{phase}.stderr').write_text(p.stderr)
            assert (p.returncode==0 if phase!='bad' else p.returncode not in (0,125)),(name,phase,p.stderr)
            data=run([RD,'--storage',W/'records','show',name+'-'+phase,'--json']).stdout
            (W/f'{name}-{phase}.show.json').write_text(data)
            r=json.loads(data);assert r['storage_integrity']['status']=='verified'
            records[phase]=dict(id=r['metadata']['id'],exit_code=r['metadata']['exit_code'],warnings=r['metadata']['warnings'])
        finally:
            if phase=='bad':restore()
    for label,right in [('normal','repeat'),('abnormal','bad')]:
        p=run([RD,'--storage',W/'records','diff',name+'-good',name+'-'+right,'--json'],check=False)
        assert p.returncode in (0,1,3),p.stdout+p.stderr
        (W/f'{name}-{label}.json').write_text(p.stdout)
        d=json.loads(p.stdout)
        ranks=[i+1 for i,c in enumerate(d['changes']) if str(clue) in c['subject']]
        RESULTS.append(dict(case=name,comparison=label,raw_changes=len(d['changes']),clue=str(clue),
                            clue_ranks=ranks,records=records,exit_code=p.returncode,reliability=d['reliability']))
        print(name,label,'changes',len(d['changes']),'clue',ranks,'exit',p.returncode,flush=True)
    (W/'results.json').write_text(json.dumps(RESULTS,indent=2))

def remove(p):
    if p.exists():shutil.rmtree(p)

try:
    run([CMAKE,'--version']);run(['cargo','--version']);run([PYTHON,'-m','pip','--version'])
    p=W/'cmake';p.mkdir();(p/'include').mkdir()
    (p/'CMakeLists.txt').write_text('cmake_minimum_required(VERSION 3.15)\nproject(fold_probe LANGUAGES CXX)\nadd_executable(probe main.cpp)\ntarget_include_directories(probe PRIVATE include)\n')
    (p/'main.cpp').write_text('#include "answer.h"\nint main() { return ANSWER == 42 ? 0 : 1; }\n')
    header=p/'include/answer.h';header.write_text('#define ANSWER 42\n')
    (p/'build.sh').write_text('#!/bin/sh\nset -eu\n"$CMAKE_BIN" -S . -B build\n"$CMAKE_BIN" --build build --parallel 2\n')
    ENV['CMAKE_BIN']=str(CMAKE)
    experiment('cmake',p,['sh','build.sh'],lambda:remove(p/'build'),lambda:header.rename(p/'saved-header'),lambda:(p/'saved-header').rename(header),header)
    p=W/'cargo';p.mkdir();(p/'src').mkdir()
    (p/'Cargo.toml').write_text('[package]\nname="fold_probe"\nversion="0.1.0"\nedition="2021"\n')
    (p/'src/main.rs').write_text('fn main() { println!("{}", include_str!("build-input")); }\n')
    dep=p/'src/build-input';dep.write_text('release metadata\n')
    run(['cargo','generate-lockfile','--offline'],p)
    experiment('cargo',p,['cargo','build','--offline','--jobs','1'],lambda:remove(p/'target'),lambda:dep.rename(p/'saved-input'),lambda:(p/'saved-input').rename(dep),dep)
    p=W/'pip';p.mkdir();wheel=p/'fixture_payload-0.1-py3-none-any.whl'
    files={'fixture_payload/__init__.py':b'value = 42\n',
           'fixture_payload-0.1.dist-info/METADATA':b'Metadata-Version: 2.1\nName: fixture-payload\nVersion: 0.1\n',
           'fixture_payload-0.1.dist-info/WHEEL':b'Wheel-Version: 1.0\nGenerator: local-fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n'}
    table=io.StringIO();writer=csv.writer(table,lineterminator='\n')
    for name,content in files.items():writer.writerow([name,'sha256='+base64.urlsafe_b64encode(hashlib.sha256(content).digest()).decode().rstrip('='),len(content)])
    writer.writerow(['fixture_payload-0.1.dist-info/RECORD','','']);files['fixture_payload-0.1.dist-info/RECORD']=table.getvalue().encode()
    with zipfile.ZipFile(wheel,'w') as z:
        for name,content in files.items():z.writestr(name,content)
    experiment('pip',p,[PYTHON,'-m','pip','install','--no-index','--no-deps','--no-compile','--target','installed',wheel.name],lambda:remove(p/'installed'),lambda:wheel.chmod(0),lambda:wheel.chmod(0o600),wheel)
finally:
    assert all(hashlib.sha256((ROOT/p).read_bytes()).hexdigest()==h for p,h in frozen.items())
    (W/'results.json').write_text(json.dumps(RESULTS,indent=2))
