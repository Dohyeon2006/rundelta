#!/usr/bin/env python3
"""Read-only product audit: real tracing, OS signals, persisted evidence; no product changes.
Linux x86_64, cc, Python3, strace. Failures of desired behavior are logged as findings.
"""
import json, os, pathlib, re, signal, subprocess, tempfile, time, sys
from cwd_tree_warning_contract import audit_directory_warnings
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=pathlib.Path(os.environ.get('RUNDELTA_BIN',str(ROOT/'target/debug/rundelta')))
WORK=pathlib.Path(tempfile.mkdtemp(prefix='audit-',dir=ROOT/'target'))
STORE=WORK/'records'
ENV=os.environ.copy()
ENV.setdefault('RUNDELTA_STRACE',str(ROOT/'target/tools/strace-6.16/src/strace'))
subprocess.run(['cc',str(ROOT/'tests/fixtures/audit_probe.c'),'-o',str(WORK/'probe')],check=True)
PROBE=WORK/'probe'
TRANSCRIPT=[]
RESULTS=[]
HANDLES=[]
ACTIVE=[]
PIDFDS={}

def command(args, expected=None, env=None):
    cmd=[str(BIN),'--storage',str(STORE),*map(str,args)]
    p=subprocess.run(cmd,env=env or ENV,text=True,capture_output=True,timeout=10)
    TRANSCRIPT.append({'command':cmd,'returncode':p.returncode,'stdout':p.stdout,'stderr':p.stderr})
    if expected is not None:assert p.returncode==expected,(args,p.returncode,p.stdout,p.stderr)
    return p

def record(name,*args,expected=0,env=None):
    return command(['record',name,'--',PROBE,*args],expected,env)

def read(name):
    return json.loads(command(['show',name,'--json'],0).stdout)

def result(name,data):
    RESULTS.append({'test':name,**data});print(name+': '+json.dumps(data,ensure_ascii=False))

def status(name,rc):
    m=read(name)['metadata']
    return dict(returncode=rc,**{k:m[k] for k in ['state','completeness','exit_code','signal','collector_exit_code','collector_signal','warnings']})

def warning_text(value):
    if isinstance(value,str):return value
    if isinstance(value,dict):return ' '.join(warning_text(item) for item in value.values())
    if isinstance(value,(list,tuple)):return ' '.join(warning_text(item) for item in value)
    return ''

def raw_trace_lines(value):
    lines=set()
    if isinstance(value,dict):
        has_trace=any(isinstance(item,str) and item.endswith('raw/trace.log')
                      for item in value.values())
        if has_trace:
            for key,item in value.items():
                if 'line' in str(key).lower():
                    if isinstance(item,int):lines.add(item)
                    elif isinstance(item,str) and item.isdecimal():lines.add(int(item))
                    elif isinstance(item,(list,tuple)):
                        lines.update(int(number) for number in item if str(number).isdecimal())
        for item in value.values():lines.update(raw_trace_lines(item))
    elif isinstance(value,(list,tuple)):
        for item in value:lines.update(raw_trace_lines(item))
    elif isinstance(value,str):
        for match in re.finditer(r'raw/trace\.log\D{0,12}(\d+)',value):lines.add(int(match.group(1)))
    return lines

def decode_trace_hex(line):
    return re.sub(r'\\x([0-9a-fA-F]{2})',lambda match:chr(int(match.group(1),16)),line)

def start(name):
    ready=WORK/(name+'.ready')
    out=(WORK/(name+'.stdout')).open('w');err=(WORK/(name+'.stderr')).open('w');HANDLES.extend([out,err])
    cmd=[str(BIN),'--storage',str(STORE),'record',name,'--',str(PROBE),'wait',str(ready)]
    p=subprocess.Popen(cmd,env=ENV,stdout=out,stderr=err)
    ACTIVE.append((p,None,None))
    deadline=time.monotonic()+5
    while True:
        if ready.exists():
            marker=ready.read_text(encoding='ascii')
            if marker.endswith('\n') and marker.count('\n')==1:
                digits=marker[:-1]
                if digits.isascii() and digits.isdecimal() and int(digits)>0:
                    target=int(digits)
                    break
        assert time.monotonic()<deadline and p.poll() is None,'target readiness timeout'
        time.sleep(.01)
    children=pathlib.Path(f'/proc/{p.pid}/task/{p.pid}/children').read_text().split()
    assert len(children)==1 and children[0].isascii() and children[0].isdecimal() and int(children[0])>0,children
    tracer=int(children[0]);ACTIVE[-1]=(p,tracer,target)
    for pid in [p.pid,tracer,target]:PIDFDS[pid]=os.pidfd_open(pid)
    TRANSCRIPT.append({'command':cmd,'recorder_pid':p.pid,'tracer_pid':tracer,'target_pid':target})
    return p,tracer,target

def kill(pid,sig):
    try:signal.pidfd_send_signal(PIDFDS[pid],sig)
    except ProcessLookupError:pass

def alive(pid):
    try:return pathlib.Path(f'/proc/{pid}/stat').read_text().split(') ',1)[1][0]!='Z'
    except FileNotFoundError:return False

try:
    record('normal','ok')
    for sig in [signal.SIGTERM,signal.SIGINT,signal.SIGKILL]:
        name=f'target-{sig.name}'
        p=record(name,'signal',str(sig.value),expected=128+sig.value)
        data=status(name,p.returncode)
        assert data['state']=='completed' and data['signal']==sig.value
        assert data['completeness']=='complete_for_supported_events'
        result(name,data)
    p=record('numeric143','exit143',expected=143)
    data=status('numeric143',p.returncode);assert data['exit_code']==143 and data['signal'] is None
    result('numeric143',data)
    # Deliberately test an unsupported signal, preserving the real outcome as a finding.
    p=command(['record','target-realtime','--',PROBE,'realtime'])
    data=status('target-realtime',p.returncode)
    passed=data['state']=='completed' and data['signal']==signal.SIGRTMIN and p.returncode==128+signal.SIGRTMIN
    assert passed,data
    result('realtime-signal',data|{'desired_behavior_pass':passed})
    p,tracer,target=start('collector-killed')
    kill(tracer,signal.SIGKILL);rc=p.wait(timeout=5)
    data=status('collector-killed',rc)
    assert rc==125 and data['state']=='capture_failed' and data['completeness']=='incomplete'
    result('collector-killed',data)
    # Real recorder versus real second recorder and delete, not just a synthetic flock holder.
    p,tracer,target=start('lock-holder')
    blocked=record('blocked-record','ok',expected=2)
    deleted=command(['delete','normal'],2)
    assert 'storage busy' in blocked.stderr and 'storage busy' in deleted.stderr
    assert read('normal')['metadata']['state']=='completed'
    result('live-concurrency',{'record_rc':blocked.returncode,'delete_rc':deleted.returncode,'stderr':blocked.stderr.strip()})
    kill(p.pid,signal.SIGTERM);rc=p.wait(timeout=5)
    assert rc==143
    result('recorder-SIGTERM',status('lock-holder',rc))
    record('after-term','ok')
    # Force recorder death while tracer and target are still active.
    p,tracer,target=start('recorder-killed')
    kill(p.pid,signal.SIGKILL);rc=p.wait(timeout=5)
    deadline=time.monotonic()+3
    while (alive(tracer) or alive(target)) and time.monotonic()<deadline:time.sleep(.01)
    orphan_tracer=alive(tracer);orphan_target=alive(target)
    assert not orphan_tracer and not orphan_target, 'orphan tracer/target remains alive'
    record('after-kill','ok')
    m=read('recorder-killed')['metadata']
    assert m['state']=='running' and m['completeness']=='pending'
    result('recorder-SIGKILL',{'recorder_rc':rc,'new_record_rc':0,'stored_state':m['state'],'stored_completeness':m['completeness'],'orphan_tracer_alive':orphan_tracer,'orphan_target_alive':orphan_target,'desired_behavior_pass':not (orphan_tracer or orphan_target)})
    kill(tracer,signal.SIGKILL);kill(target,signal.SIGKILL)
    # Both text and JSON must warn, even when an incomplete record is compared to itself.
    for name in ['collector-killed','lock-holder','recorder-killed']:
        text=command(['diff',name,name],3)
        data=json.loads(command(['diff',name,name,'--json'],3).stdout)
        assert 'warning:' in text.stdout and data['warnings']
        assert data['observed_differences'] is False
        assert data['reliability']['left']['capture'] in ['incomplete','unknown']
        assert data['exit_code']==3
        assert 'RELIABILITY NOT ESTABLISHED' in text.stdout
        (WORK/f'diff-{name}.txt').write_text(text.stdout)
        result('incomplete-diff-'+name,{'returncode':text.returncode,'warnings':data['warnings'],'stdout':text.stdout})
    text=command(['diff','normal','collector-killed'],3)
    assert 'Capture completeness' in text.stdout and 'incomplete' in text.stdout
    (WORK/'diff-normal-failed.txt').write_text(text.stdout)
    # Actual fork, child chdir/fchdir, failed parent chdir, and legacy open syscall.
    tree=WORK/'cwd-tree'
    for folder,file in [('parent','parent.txt'),('child','child.txt'),('fd','fd.txt')]:
        (tree/folder).mkdir(parents=True);(tree/folder/file).write_text('test')
    record('cwd-tree','cwd',tree)
    r=read('cwd-tree');files=[e for e in r['events'] if e['operation']=='open' and e['path'] and e['path'].endswith('.txt')]
    expected={str(tree/d/f) for d,f in [('parent','parent.txt'),('child','child.txt'),('fd','fd.txt')]}
    assert {os.path.normpath(e['path']) for e in files}==expected
    assert r['metadata']['completeness']=='partial'
    assert len({e['pid'] for e in files})==2

    warnings=r['metadata']['warnings']
    run_directory=STORE/'runs'/r['metadata']['id']
    raw_lines=(run_directory/'raw'/'trace.log').read_text().splitlines()
    audit_directory_warnings(warnings, raw_lines)

    annotations=re.compile(r'=\s*\d+<(/[^>]+)>')
    assert len(files)==3,files
    for event in files:
        references=raw_trace_lines(event)
        assert references,event
        assert all(1<=number<=len(raw_lines) for number in references),(event,references)
        annotated_paths=[]
        for number in references:
            line=decode_trace_hex(raw_lines[number-1])
            annotated_paths.extend(match.group(1) for match in annotations.finditer(line))
        assert any(os.path.isabs(path) and os.path.normpath(path)==os.path.normpath(event['path'])
                   for path in annotated_paths),(event,annotated_paths)
    result('parent-child-cwd',{'state':r['metadata']['state'],'completeness':r['metadata']['completeness'],'events':files,'warnings':warnings})
    # Real saved v1 from phase one when available. Format compatibility fixture below is always reproducible.
    historical=ROOT/'target/acceptance-dt7m3fto/records'
    if historical.exists():
        old=subprocess.run([str(BIN),'--storage',str(historical),'show','repeat-a','--json'],env=ENV,text=True,capture_output=True,check=True)
        TRANSCRIPT.append({'historical_v1_command':[str(BIN),'--storage',str(historical),'show','repeat-a','--json'],'stdout':old.stdout})
        r=json.loads(old.stdout);assert r['metadata']['schema_version']==1
        result('historical-v1',{'read_pass':True,'schema':1,'events':len(r['events'])})
    record('compatibility','ok')
    r=read('compatibility');d=STORE/'runs'/r['metadata']['id']
    original_m=(d/'metadata.json').read_text();original_e=(d/'events.jsonl').read_text()
    original_marker=(d/'finalized.json').read_bytes();(d/'finalized.json').unlink()
    m=json.loads(original_m);m['schema_version']=1
    for k in ['environment','completeness','collector_exit_code','collector_signal']:m.pop(k,None)
    lines=[]
    for line in original_e.splitlines():
        e=json.loads(line);e['schema_version']=1
        for k in ['argv','child_pid','cwd_before','context_uncertain']:e.pop(k,None)
        lines.append(json.dumps(e))
    (d/'metadata.json').write_text(json.dumps(m));(d/'events.jsonl').write_text('\n'.join(lines)+'\n')
    r=read('compatibility');assert r['metadata']['environment'] is None
    command(['diff','compatibility','normal','--json'],3)
    result('v1-fixture',{'read_pass':True,'environment':r['metadata']['environment'],'completeness':r['metadata']['completeness'],'note':'real capture converted to v1 field layout; not a historical capture'})
    (d/'metadata.json').write_text(original_m);(d/'events.jsonl').write_text(original_e)
    m=json.loads(original_m);m['schema_version']=2;(d/'metadata.json').write_text(json.dumps(m))
    v2=read('compatibility')
    assert v2['metadata']['schema_version']==2 and v2['storage_integrity']['status']=='unverified'
    result('v2-read',{'read_pass':True,'storage':'unverified'})
    (d/'metadata.json').write_text(original_m);(d/'finalized.json').write_bytes(original_marker)
    v3=read('compatibility')
    assert v3['metadata']['schema_version']==3 and v3['storage_integrity']['status']=='verified'
    result('v3-read',{'read_pass':True,'storage':'verified'})
    for version in [0,999]:
        m=json.loads(original_m);m['schema_version']=version;(d/'metadata.json').write_text(json.dumps(m))
        command(['show','compatibility'],2)
    (d/'metadata.json').write_text(original_m)
    # Demonstrate validation limits with a valid-JSON but truncated events file; restore afterward.
    lines=original_e.splitlines();(d/'events.jsonl').write_text('\n'.join(lines[:-1])+'\n')
    shown=command(['show','compatibility'],2)
    text=command(['diff','compatibility','compatibility'],2)
    data=json.loads(command(['diff','compatibility','compatibility','--json'],2).stdout)
    assert 'corrupt' in text.stdout and data['reliability']['left']['storage']=='corrupt'
    assert data['observed_differences'] is None and data['errors']
    result('post-save-truncated-events',{'show_passes':False,'diff_rc':text.returncode,'diff_storage':data['reliability']['left']['storage'],'desired_behavior_pass':True})
    (d/'events.jsonl').write_text(original_e)
finally:
    for p,tracer,target in ACTIVE:
        if p.poll() is None:kill(p.pid,signal.SIGTERM)
        if tracer:kill(tracer,signal.SIGKILL)
        if target:kill(target,signal.SIGKILL)
        try:p.wait(timeout=3)
        except subprocess.TimeoutExpired:kill(p.pid,signal.SIGKILL)
    for h in HANDLES:h.close()
    for fd in PIDFDS.values():os.close(fd)
    (WORK/'commands.json').write_text(json.dumps(TRANSCRIPT,ensure_ascii=False,indent=2))
    (WORK/'results.json').write_text(json.dumps(RESULTS,ensure_ascii=False,indent=2))
    print('ARTIFACTS:',WORK)

findings=[r['test'] for r in RESULTS if r.get('desired_behavior_pass') is False]
print('FINDINGS:',len(findings),', '.join(findings))
sys.exit(1 if findings else 0)
