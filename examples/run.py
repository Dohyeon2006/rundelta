#!/usr/bin/env python3
"""Real Linux/strace acceptance scenarios. Requires cc and a built rundelta."""
import json, os, pathlib, shutil, subprocess, tempfile, time, signal, sys
ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from acceptance_support import require_owned_group_context, strace_environment, run_owned_command, wait_owned_group

BIN = pathlib.Path(os.environ.get('RUNDELTA_BIN', str(ROOT / 'target/debug/rundelta')))

def main():
    work = pathlib.Path(tempfile.mkdtemp(prefix='acceptance-', dir=ROOT / 'target'))
    storage = work / 'records'
    run_owned_command(['cc', str(ROOT / 'examples/probe.c'), '-o', str(work / 'probe')], check=True, timeout=30)
    env = strace_environment(ROOT)
    env.pop("PROBE_CONFIG", None)
    def call(args, code=0, extra=None):
        p = run_owned_command([str(BIN), '--storage', str(storage), *map(str,args)], env=env | (extra or {}), text=True, capture_output=True, timeout=15)
        assert p.returncode == code, (args,p.returncode,p.stdout,p.stderr)
        return p
    def record(name, *cmd, code=0, extra=None):
        return call(['record',name,'--',*cmd], code, extra)
    def diff(left,right,changed=True):
        p=call(['diff',left,right,'--json'],int(changed))
        data=json.loads(p.stdout)
        text=call(['diff',left,right],int(changed)).stdout
        (work / f'{left}-{right}.txt').write_text(text)
        print(text)
        return data
    for folder in ['a','b']:
        (work/folder).mkdir(); shutil.copy2(work/'probe',work/folder/'probe')
    record('exe-good','probe','ok',extra={'PATH':str(work/'a')})
    record('exe-bad','probe','ok',extra={'PATH':str(work/'b')})
    d=diff('exe-good','exe-bad')
    assert len([c for c in d['changes'] if c['category']=='Executable changes'])==2
    for name in ['old.conf','new.conf']:(work/name).write_text('config')
    record('config-good',work/'probe','config',extra={'PROBE_CONFIG':str(work/'old.conf')})
    record('config-bad',work/'probe','config',extra={'PROBE_CONFIG':str(work/'new.conf')})
    d=diff('config-good','config-bad')
    assert any(c['category']=='New file access' and 'new.conf' in c['subject'] for c in d['changes'])
    asset=work/'schema.json';asset.write_text('{}')
    record('missing-good',work/'probe',asset)
    asset.unlink()
    record('missing-bad',work/'probe',asset,code=1)
    d=diff('missing-good','missing-bad')
    assert any(c['left']==['success'] and c['right']==['ENOENT'] and 'schema.json' in c['subject'] for c in d['changes'])
    record('repeat-a',work/'probe','ok');record('repeat-b',work/'probe','ok')
    diff('repeat-a','repeat-b',False)
    record('child',work/'probe','child')
    special=work/'space " quote';special.write_text('x')
    record('special',work/'probe',special)
    (work/'relative.txt').write_text('x');record('cwd',work/'probe','cwd',work)
    record('fchdir',work/'probe','fchdir',work)
    record('signal',work/'probe','signal',code=143)
    record('repeat-a',work/'probe','ok',code=2)
    marker=work/'must-not-exist'
    record('no-strace','/usr/bin/touch',marker,code=2,extra={'RUNDELTA_STRACE':str(work/'absent-strace')})
    assert not marker.exists()
    record('unusable-strace','/usr/bin/touch',marker,code=2,extra={'RUNDELTA_STRACE':'/usr/bin/false'})
    assert not marker.exists()
    # Interrupt the recorder itself, not just the traced target.
    require_owned_group_context()
    p=subprocess.Popen([str(BIN),'--storage',str(storage),'record','interrupted','--',str(work/'probe'),'wait'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
    try:
        deadline=time.monotonic()+5
        while time.monotonic()<deadline:
            logs=list((storage/'runs').glob('*/raw/trace.log'))
            if any('\\x77\\x61\\x69\\x74' in log.read_text() for log in logs): break
            time.sleep(0.01)
        else:
            raise AssertionError('waiting target did not start')
        p.send_signal(signal.SIGTERM)
        wait_owned_group(p, 5)
        p.communicate(timeout=2)
    finally:
        if p.returncode is None:
            # Retain the direct-child identity until private-group cleanup. A
            # readiness or communication failure must not leave owned children.
            try:
                wait_owned_group(p, 0.001)
            except subprocess.TimeoutExpired:
                pass
        for stream in (p.stdout, p.stderr):
            if stream is not None:
                stream.close()
    assert p.returncode==143,p.returncode
    runs={}
    for path in (storage/'runs').iterdir():
        m=json.loads((path/'metadata.json').read_text());runs[m['name']]=(path,m)
    assert runs['interrupted'][1]['state']=='interrupted'
    assert runs['child'][1]['state']=='completed'
    assert any(json.loads(line)['kind']=='spawn' for line in (runs['child'][0]/'events.jsonl').read_text().splitlines())
    assert not runs['cwd'][1]['warnings']
    assert any(json.loads(line).get('path')==str(work/'relative.txt') for line in (runs['cwd'][0]/'events.jsonl').read_text().splitlines())
    for name in ['exe-good','exe-bad','config-good','config-bad','missing-good','missing-bad','repeat-a','repeat-b','special']:
        assert runs[name][1]['state']=='completed',runs[name]
        assert not runs[name][1]['warnings'],runs[name]
    path,m=runs['repeat-b'];original_metadata=(path/'metadata.json').read_text();m['schema_version']=999
    (path/'metadata.json').write_text(json.dumps(m));call(['diff','repeat-a','repeat-b'],2)
    m['schema_version']=3;(path/'metadata.json').write_text(original_metadata)
    original=(path/'events.jsonl').read_text();(path/'events.jsonl').write_text('{broken\n');call(['diff','repeat-a','repeat-b'],2);(path/'events.jsonl').write_text(original)
    # Stage two: persisted metadata, CLI management, selected startup environment.
    listing=json.loads(call(['list','--json']).stdout)
    assert len(listing)==len(runs)
    child=json.loads(call(['show','child','--json']).stdout)
    assert any(e['child_pid'] for e in child['events'] if e['kind']=='spawn')
    assert any(e['argv']==[str(work/'probe'),'ok'] for e in child['events'] if e['kind']=='exec')
    assert runs['fchdir'][1]['completeness']=='complete_for_supported_events'
    assert runs['interrupted'][1]['completeness']=='incomplete'
    assert runs['signal'][1]['state']=='completed'
    assert runs['signal'][1]['signal']==15
    assert runs['signal'][1]['completeness']=='complete_for_supported_events'
    env.pop('RUNDELTA_TEST_UNSET',None)
    for name,value in [('env-a','alpha'),('env-b','beta')]:
        call(['record',name,'--env','RUNDELTA_TEST_VALUE','--env','RUNDELTA_TEST_UNSET','--',work/'probe','ok'],extra={'RUNDELTA_TEST_VALUE':value,'DO_NOT_CAPTURE':'secret'})
    a=json.loads(call(['show','env-a','--json']).stdout)
    assert a['metadata']['environment']=={'RUNDELTA_TEST_VALUE':'alpha','RUNDELTA_TEST_UNSET':None}
    d=json.loads(call(['diff','env-a','env-b','--json'],1).stdout)
    assert len(d['changes'])==1 and d['changes'][0]['subject']=='RUNDELTA_TEST_VALUE'
    call(['record','env-empty','--env','RUNDELTA_TEST_UNSET','--',work/'probe','ok'],extra={'RUNDELTA_TEST_UNSET':''})
    empty=json.loads(call(['show','env-empty','--json']).stdout)
    assert empty['metadata']['environment']['RUNDELTA_TEST_UNSET']==''
    call(['record','invalid-env','--env','A=B','--',work/'probe','ok'],2)
    # A path-looking label stays a label; delete by ID only removes the selected record.
    record('../path-label',work/'probe','ok')
    label=json.loads(call(['show','../path-label','--json']).stdout)
    call(['delete',label['metadata']['id']]);call(['show','../path-label'],2)
    assert work.exists() and marker.exists()==False
    call(['delete','env-b']);call(['show','env-b'],2)
    # Compatibility: v1 fields really absent, not merely a changed version number.
    legacy=work/'records'/'runs'/runs['repeat-b'][1]['id']
    saved_metadata=(legacy/'metadata.json').read_text();metadata=json.loads(saved_metadata)
    saved_marker=(legacy/'finalized.json').read_bytes();(legacy/'finalized.json').unlink()
    for key in ['environment','completeness','collector_exit_code','collector_signal']:metadata.pop(key,None)
    metadata['schema_version']=1;(legacy/'metadata.json').write_text(json.dumps(metadata))
    old_events=[]
    for line in (legacy/'events.jsonl').read_text().splitlines():
        event=json.loads(line);event['schema_version']=1
        for key in ['argv','child_pid','cwd_before','context_uncertain']:event.pop(key,None)
        old_events.append(json.dumps(event))
    (legacy/'events.jsonl').write_text('\n'.join(old_events)+'\n')
    legacy_show=json.loads(call(['show','repeat-b','--json']).stdout)
    assert legacy_show['metadata']['environment'] is None
    assert legacy_show['storage_integrity']['status']=='unverified'
    (legacy/'metadata.json').write_text(saved_metadata);(legacy/'events.jsonl').write_text(original);(legacy/'finalized.json').write_bytes(saved_marker)
    # Stale lock file does not block new records; a live lock does block deletion.
    import fcntl
    with (storage/'.record.lock').open('r+') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        call(['delete','env-a'],2)
        record('locked',work/'probe','ok',code=2)
    record('after-lock',work/'probe','ok')
    # A trace that cannot launch the command still leaves a readable failed record.
    record('failed-launch',work/'absent-command',code=125)
    failed=json.loads(call(['show','failed-launch','--json']).stdout)
    assert failed['metadata']['state']=='capture_failed'
    assert failed['metadata']['exit_code'] is None
    call(['delete','failed-launch'])
    print(f'PASS: real capture acceptance and edge cases\nArtifacts: {work}')
if __name__=='__main__':main()
