#!/usr/bin/env python3
"""Rebuild the public experiment in a NEW temporary directory.
Requires uv, an existing RunDelta binary, strace, make and a C++ compiler.
Fetches only the issue body API (never comments); source snippets and all tools,
venvs, build outputs and raw captures stay outside this repository.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
RD = Path(os.environ.get('RUNDELTA_BIN', ROOT / 'target/debug/rundelta')).resolve()
UV = os.environ.get('UV_BIN') or shutil.which('uv')
TRACER = os.environ.get('RUNDELTA_STRACE') or shutil.which('strace')
if not UV or not TRACER or not RD.is_file():
    raise SystemExit('Set UV_BIN, RUNDELTA_STRACE and RUNDELTA_BIN to existing executables; see README.md.')
W = Path(tempfile.mkdtemp(prefix='rundelta-external-'))
print('Private artifacts:', W, flush=True)
env = os.environ.copy()
for k in ['PYTHONHOME','PYTHONPATH','VIRTUAL_ENV','CPATH','CPLUS_INCLUDE_PATH','C_INCLUDE_PATH',
          'CXXFLAGS','CFLAGS','LDFLAGS','CMAKE_PREFIX_PATH','LD_LIBRARY_PATH']:
    env.pop(k, None)
env.update(UV_CACHE_DIR=str(W/'cache'), UV_PYTHON_INSTALL_DIR=str(W/'pythons'),
           RUNDELTA_STRACE=str(Path(TRACER).resolve()), LC_ALL='C', PYTHONDONTWRITEBYTECODE='1')
commands = []

def run(argv, cwd=W, environ=None, check=True):
    p = subprocess.run(list(map(str, argv)), cwd=cwd, env=environ or env,
                       text=True, capture_output=True, timeout=300)
    commands.append(dict(argv=list(map(str, argv)), cwd=str(cwd), returncode=p.returncode,
                         stdout=p.stdout, stderr=p.stderr))
    (W/'commands.json').write_text(json.dumps(commands, indent=2))
    if check and p.returncode:
        raise RuntimeError(f'Command failed ({p.returncode}); see private commands.json: {argv[0]}')
    return p

pin = json.loads((HERE/'evidence.json').read_text())
request = urllib.request.Request('https://api.github.com/repos/pybind/pybind11/issues/5626',
                                 headers={'User-Agent':'RunDelta-public-reproduction'})
with urllib.request.urlopen(request, timeout=60) as response:
    issue = json.load(response)
blocks = re.findall(r'```(?:cpp|cmake)\n(.*?)```', issue['body'], re.S)
for kind, snippet in zip(['cpp','cmake'], blocks[:2]):
    if hashlib.sha256(snippet.encode()).hexdigest() != pin['issue_snippet_sha256'][kind]:
        raise SystemExit('Issue snippet changed; review source rather than silently updating the frozen case.')
consumer=W/'consumer'; consumer.mkdir()
(consumer/'min_example.cpp').write_text(blocks[0])
(consumer/'CMakeLists.txt').write_text(blocks[1])
(consumer/'run-case.sh').write_text('''#!/usr/bin/env bash
set -eu
cmake -S . -B build -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build build --parallel 2
cd build
python -c 'import sys; print("runtime:", sys.executable, sys.version); import min_example; print("result:", min_example.do_something())'
''')
run([UV,'--version'])
run([UV,'python','install','3.10.19','3.12.10','--no-bin'])
for version, tag in [('3.10.19','310'),('3.12.10','312')]:
    python=next((W/'pythons').glob('cpython-'+version+'-*/bin/python3'))
    run([UV,'venv','--python',python,W/('venv'+tag)])
    run([UV,'pip','install','--python',W/('venv'+tag)/'bin/python','pybind11[global]==2.13.6'])
run([UV,'venv','--python',W/'venv312/bin/python',W/'build-tools'])
run([UV,'pip','install','--python',W/'build-tools/bin/python','cmake==3.31.2'])
header=next((W/'pythons').glob('cpython-3.10.19-*/include/python3.10'))
for name, tag, polluted, expected in [('native310','310',False,0),('native312','312',False,0),
                                     ('controlled312','312',True,1),('native312repeat','312',False,0)]:
    if (consumer/'build').exists(): shutil.rmtree(consumer/'build')
    e=env | {'VIRTUAL_ENV':str(W/('venv'+tag)), 'CMAKE_PREFIX_PATH':str(W/('venv'+tag)),
             'PATH':str(W/('venv'+tag)/'bin')+':'+str(W/'build-tools/bin')+':'+env['PATH']}
    if polluted: e['CPATH']=str(header)
    p=run([RD,'--storage',W/'records','record',name,'--','bash','run-case.sh'],consumer,e,False)
    (W/(name+'.stdout')).write_text(p.stdout); (W/(name+'.stderr')).write_text(p.stderr)
    assert p.returncode == expected, (name,p.returncode,'inspect private logs; do not retune')
    if polluted: assert 'Python version mismatch' in p.stderr
    print(name, 'exit', p.returncode, flush=True)
for left,right in [('native310','controlled312'),('native312','controlled312'),('native312','native312repeat')]:
    args=[RD,'--storage',W/'records','diff',left,right]
    p=run(args+['--json'],check=False); d=json.loads(p.stdout)
    text=run(args,check=False); assert p.returncode==text.returncode==d['exit_code']
    (W/(left+'--'+right+'.json')).write_text(p.stdout)
    (W/(left+'--'+right+'.txt')).write_text(text.stdout)
    ranks=[i+1 for i,c in enumerate(d['changes']) if c['subject'].endswith('/Python.h (openat)')]
    print(left, right, 'changes',len(d['changes']),'Python.h ranks',ranks,'exit',p.returncode)
print('Finished. This is controlled header drift, not proof of the upstream issue. Do not publish private outputs wholesale.')
