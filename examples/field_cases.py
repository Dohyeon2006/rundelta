#!/usr/bin/env python3
"""Five real-tool failure reproductions. No network, product changes, or synthetic traces.
Requires the configured strace source tree used during development, system pycparser
examples, Python venv, gcc, GNU make, pkg-config/libcrypt, git, and offline Cargo cache.
"""
import json, os, pathlib, shlex, shutil, subprocess, tempfile, time
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=pathlib.Path(os.environ.get('RUNDELTA_BIN',str(ROOT/'target/debug/rundelta'))).resolve()
SOURCE=pathlib.Path(os.environ.get('RUNDELTA_STRACE_SOURCE',str(ROOT/'target/tools/strace-6.16'))).resolve()
TRACER=pathlib.Path(os.environ.get('RUNDELTA_STRACE',str(SOURCE/'src/strace'))).resolve()
EXAMPLES=pathlib.Path('/usr/share/doc/python3-pycparser/examples')
WORK=pathlib.Path(tempfile.mkdtemp(prefix='field-cases-',dir=ROOT/'target'))
STORE=WORK/'records'
BASE=os.environ.copy()
BASE.update(RUNDELTA_STRACE=str(TRACER),LC_ALL='C',PYTHONDONTWRITEBYTECODE='1',CARGO_NET_OFFLINE='true')
for key in ['PYTHONHOME','PYTHONPATH','PKG_CONFIG_PATH','PKG_CONFIG_LIBDIR','RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER']:
    BASE.pop(key,None)
for key in list(BASE):
    if key.startswith(('GIT_CONFIG','GIT_AUTHOR_','GIT_COMMITTER_')) or key in ['GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE']:
        BASE.pop(key)
COMMANDS=[]
RESULTS=[]

def setup(cmd,cwd=None,env=None):
    p=subprocess.run(list(map(str,cmd)),cwd=cwd,env=env or BASE,text=True,capture_output=True,timeout=180)
    COMMANDS.append(dict(phase='setup',command=list(map(str,cmd)),cwd=str(cwd or ROOT),returncode=p.returncode,stdout=p.stdout,stderr=p.stderr))
    if p.returncode:raise RuntimeError((cmd,p.returncode,p.stdout[-3000:],p.stderr[-3000:]))
    return p.stdout

def record(case,phase,cmd,cwd,env):
    name=f'{case}-{phase}'
    invocation=[str(BIN),'--storage',str(STORE),'record',name,'--',*map(str,cmd)]
    start=time.monotonic()
    p=subprocess.run(invocation,cwd=cwd,env=env,text=True,capture_output=True,timeout=180)
    elapsed=time.monotonic()-start
    (WORK/f'{name}.stdout').write_text(p.stdout)
    (WORK/f'{name}.stderr').write_text(p.stderr)
    COMMANDS.append(dict(phase='record',name=name,command=invocation,cwd=str(cwd),returncode=p.returncode,seconds=elapsed,stdout_file=f'{name}.stdout',stderr_file=f'{name}.stderr'))
    if phase!='bad':assert p.returncode==0,(name,p.returncode,p.stderr[-4000:])
    else:assert p.returncode not in [0,125],(name,p.returncode,p.stderr[-4000:])
    out=setup([BIN,'--storage',STORE,'show',name,'--json'])
    run=json.loads(out)
    assert run['storage_integrity']['status']=='verified',name
    assert run['metadata']['state']=='completed',(name,run['metadata'])
    return run

def diff(left,right,outname):
    p=subprocess.run([str(BIN),'--storage',str(STORE),'diff',left,right,'--json'],env=BASE,text=True,capture_output=True,timeout=60)
    assert p.returncode in [0,1,3],(left,right,p.stdout,p.stderr)
    data=json.loads(p.stdout)
    (WORK/f'{outname}.json').write_text(p.stdout)
    text=subprocess.run([str(BIN),'--storage',str(STORE),'diff',left,right],env=BASE,text=True,capture_output=True,timeout=60)
    assert text.returncode==p.returncode
    (WORK/f'{outname}.txt').write_text(text.stdout)
    COMMANDS.append(dict(phase='diff',left=left,right=right,returncode=p.returncode,json_file=f'{outname}.json',text_file=f'{outname}.txt'))
    return data

def experiment(case,cmd,cwd,good_env,bad_env,prepare,mutate,restore,clue_fragments,description):
    print(f'START {case}',flush=True)
    prepare();good=record(case,'good',cmd,cwd,good_env)
    prepare();repeat=record(case,'repeat',cmd,cwd,good_env)
    prepare();mutate()
    try:bad=record(case,'bad',cmd,cwd,bad_env)
    finally:restore()
    control=diff(case+'-good',case+'-repeat',case+'-control')
    report=diff(case+'-good',case+'-bad',case+'-comparison')
    # Predeclared diagnostic paths, not post-hoc promotion of downstream effects.
    ranks=[i+1 for i,c in enumerate(report['changes']) if any(f in c['subject'] for f in clue_fragments)]
    result=dict(case=case,description=description,command=list(map(str,cmd)),cwd=str(cwd),
        clue_fragments=clue_fragments,clue_ranks=ranks,first_useful_rank=min(ranks) if ranks else None,
        top5_hit=any(r<=5 for r in ranks),total_changes=len(report['changes']),control_changes=len(control['changes']),
        reliability=report['reliability'],diff_exit_code=report['exit_code'],
        runs={k:dict(id=r['metadata']['id'],exit_code=r['metadata']['exit_code'],warnings=r['metadata']['warnings'],events=r['metadata']['parsed_events']) for k,r in [('good',good),('repeat',repeat),('bad',bad)]},
        top5=report['changes'][:5])
    RESULTS.append(result)
    print(f'FINISH {case}: total={result["total_changes"]}, control={result["control_changes"]}, first_useful={result["first_useful_rank"]}, top5_hit={result["top5_hit"]}',flush=True)
    (WORK/'results.json').write_text(json.dumps(RESULTS,indent=2))

try:
    setup([BIN,'--version']);setup([TRACER,'--version'])
    for tool in ['python3','gcc','make','pkg-config','git','cargo']:
        setup([tool,'--version'])
    assert EXAMPLES.joinpath('c-to-c.py').exists()
    assert SOURCE.joinpath('src/Makefile').exists()
    # 1: Run the installed pycparser project's real C translation example.
    case=WORK/'python-venv';case.mkdir()
    for name,system in [('system-env',True),('isolated-env',False)]:
        cmd=['/usr/bin/python3','-m','venv','--without-pip']
        if system:cmd+=['--system-site-packages']
        setup([*cmd,case/name])
    good=BASE|{'PATH':str(case/'system-env/bin')+':'+BASE['PATH']}
    bad=BASE|{'PATH':str(case/'isolated-env/bin')+':'+BASE['PATH']}
    experiment('python-venv',['python3',EXAMPLES/'c-to-c.py',EXAMPLES/'c_files/basic.c'],case,good,bad,
        lambda:None,lambda:None,lambda:None,['/system-env/bin/python3','/isolated-env/bin/python3','/pycparser/'],
        'PATH selects an isolated Python venv that cannot import the installed pycparser dependency')
    # 2: Resolve an actually installed development dependency, then remove its search location.
    case=WORK/'pkg-config';case.mkdir();(case/'empty-pc').mkdir()
    pcdir=setup(['pkg-config','--variable=pcfiledir','libcrypt']).strip()
    assert pathlib.Path(pcdir,'libcrypt.pc').exists()
    experiment('pkg-config',['pkg-config','--cflags','--libs','libcrypt'],case,BASE,BASE|{'PKG_CONFIG_LIBDIR':str(case/'empty-pc')},
        lambda:None,lambda:None,lambda:None,['/libcrypt.pc','/empty-pc'],
        'PKG_CONFIG_LIBDIR replaces the system search path; the installed libcrypt package becomes undiscoverable')
    # 3: Actual Git commit with explicit identity requirement and a disappearing user config.
    case=WORK/'git-identity';case.mkdir();home=case/'home';home.mkdir();config=home/'.gitconfig'
    config.write_text('[user]\n\tname = RunDelta Evaluation\n\temail = evaluation@example.invalid\n')
    repo=case/'repo'
    env=BASE|{'HOME':str(home),'XDG_CONFIG_HOME':str(home/'xdg'),'GIT_CONFIG_NOSYSTEM':'1',
              'GIT_AUTHOR_DATE':'2026-01-01T00:00:00+0000','GIT_COMMITTER_DATE':'2026-01-01T00:00:00+0000'}
    def fresh_repo():
        if repo.exists():shutil.rmtree(repo)
        setup(['git','init','--quiet',repo],env=env)
    experiment('git-identity',['git','-c','user.useConfigOnly=true','commit','--allow-empty','-m','Build environment check'],repo,env,env,
        fresh_repo,lambda:config.rename(home/'.gitconfig.saved'),lambda:(home/'.gitconfig.saved').rename(config),['/home/.gitconfig'],
        'Git commit fails after the only configured author identity file disappears')
    # 4: Compile actual strace.c with its configured GCC command; lose a real required header.
    case=WORK/'gcc-header';case.mkdir()
    copied=case/'strace-source'
    shutil.copytree(SOURCE,copied,ignore=shutil.ignore_patterns('*.o','*.a','strace'))
    dry=setup(['make','-n','-W','strace.c','strace-strace.o'],cwd=copied/'src')
    line=next(line for line in dry.splitlines() if line.startswith('gcc ') and '-c -o strace-strace.o' in line)
    line=line.replace("`test -f 'strace.c' || echo './'`strace.c",'strace.c')
    assert '`' not in line,line
    compiler=shlex.split(line)
    header=copied/'src/defs.h';backup=header.with_suffix('.h.saved')
    def clean_object():
        (copied/'src/strace-strace.o').unlink(missing_ok=True)
        (copied/'src/.deps/strace-strace.Tpo').unlink(missing_ok=True)
    experiment('gcc-header',compiler,copied/'src',BASE,BASE,clean_object,
        lambda:header.rename(backup),lambda:backup.rename(header),['/defs.h'],
        'The configured GCC compilation of real strace.c fails because its required defs.h was removed')
    # 5: Build RunDelta itself offline, with a now-invalid target linker override.
    case=WORK/'cargo-linker';case.mkdir();out=case/'build';(case/'tmp').mkdir()
    host=next(line.split(': ',1)[1] for line in setup(['rustc','-vV']).splitlines() if line.startswith('host: '))
    key='CARGO_TARGET_'+host.upper().replace('-','_')+'_LINKER'
    good=BASE|{'TMPDIR':str(case/'tmp'),key:'cc'}
    bad=BASE|{'TMPDIR':str(case/'tmp'),key:str(case/'missing-linker')}
    def clean_cargo():
        if out.exists():shutil.rmtree(out)
        for item in (case/'tmp').iterdir():
            if item.is_dir():shutil.rmtree(item)
            else:item.unlink()
    experiment('cargo-linker',['cargo','build','--offline','--jobs','1','--manifest-path',ROOT/'Cargo.toml','--target-dir',out],ROOT,good,bad,
        clean_cargo,lambda:None,lambda:None,['/missing-linker'],
        f'{key} points to a nonexistent linker during an actual offline build of RunDelta')
finally:
    (WORK/'commands.json').write_text(json.dumps(COMMANDS,indent=2))
    (WORK/'results.json').write_text(json.dumps(RESULTS,indent=2))
    print('ARTIFACTS:',WORK,flush=True)
