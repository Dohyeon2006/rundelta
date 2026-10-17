#!/usr/bin/env python3
"""Measure unchanged RunDelta using synthetic input and actual persistence paths.
No product/source edits; all raw inputs, records and tool output stay outside repo.
Run from any directory: python3 -B measure.py --binary /absolute/rundelta
"""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import statistics
import subprocess
import tempfile
import time

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]

def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def git(*args):return subprocess.check_output(['git',*args],cwd=ROOT,text=True).strip()
def stats(values):
    return dict(min=min(values),median=statistics.median(values),max=max(values),
                sample_stdev=statistics.stdev(values) if len(values)>1 else 0)

def fixture(path,events,processes):
    assert events>=2*processes
    pids=[str(10000+i) for i in range(processes)]
    lines=[];occurrences=Counter();exit_positions={}
    def emit(pid,text,child=None):
        lines.append(pid+' '+text+'\n');occurrences[pid]+=1
        if child:occurrences[child]+=1
    emit(pids[0],'execve("/synthetic/target", [], 0) = 0')
    for pid in pids[1:]:emit(pids[0],'fork() = '+pid,child=pid)
    for i in range(events-2*processes):emit(pids[i%processes],'open("/synthetic/input", O_RDONLY) = 3')
    for pid in pids[1:]+pids[:1]:
        emit(pid,'+++ exited with 0 +++');exit_positions[pid]=len(lines)
    assert len(lines)==events
    path.write_text(''.join(lines))
    return dict(events=events,processes=processes,spawn_events=processes-1,
                pid_candidates=sum(occurrences.values()),
                predicted_any_predicate_visits=sum(occurrences[p]*exit_positions[p] for p in pids),
                trace_sha256=sha(path))

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--repeats',type=int,default=3)
    args=parser.parse_args();assert args.repeats>=3
    binary=args.binary.resolve();assert binary.is_file()
    capture=ROOT/'src/capture/mod.rs'
    lines=capture.read_text().splitlines()
    assert 'let missing_exits:' in lines[197] and 'if !missing_exits.is_empty()' in lines[208]
    assert 'eprintln!' in lines[257], 'review breakpoint mapping before running on changed source'
    tracked=git('ls-files','-z').split('\0')
    before={p:sha(ROOT/p) for p in tracked if p and (ROOT/p).is_file()}
    work=Path(tempfile.mkdtemp(prefix='rundelta-a3-matrix-'))
    print('PRIVATE_ARTIFACTS:',work,flush=True)
    base=os.environ.copy();base['RUNDELTA_STRACE']=str(HERE/'synthetic_backend.py')
    result=dict(schema_version=1,head=git('rev-parse','HEAD'),profile='existing cargo dev binary; opt-level=0; debug symbols',
                binary_sha256=sha(binary),capture_sha256=sha(capture),plan_sha256=sha(HERE/'PLAN.md'),
                script_sha256={p.name:sha(p) for p in [HERE/'measure.py',HERE/'probe_gdb.py',HERE/'synthetic_backend.py']},
                environment=dict(kernel=platform.release(),machine=platform.machine(),python=platform.python_version(),
                    rustc=subprocess.check_output(['rustc','--version'],text=True).strip(),
                    gdb=subprocess.check_output(['gdb','--version'],text=True).splitlines()[0],
                    cpu=next(x.split(':',1)[1].strip() for x in Path('/proc/cpuinfo').read_text().splitlines() if x.startswith('model name')),
                    logical_cpus=os.cpu_count(),affinity_cpus=sorted(os.sched_getaffinity(0)),
                    timing_clock='perf_counter_ns; wall-time',rss_units='KiB',repeats=args.repeats),
                samples=[],groups=[],failures=[])
    matrix=[(e,p) for e in [2000,8000,32000] for p in [1,16,128]]
    fixtures={}
    for e,p in matrix:
        f=work/f'trace-{e}-{p}';fixtures[e,p]=fixture(f,e,p)
    def command(cmd,env,log,stdout=subprocess.DEVNULL):
        started=time.perf_counter_ns()
        with log.open('w') as err:
            p=subprocess.run(cmd,env=env,cwd='/',stdout=stdout,stderr=err,timeout=180)
        elapsed=(time.perf_counter_ns()-started)/1e6
        assert p.returncode==0,(cmd[0],p.returncode,str(log))
        return elapsed
    def record(e,p,label,debug):
        folder=work/label;folder.mkdir();store=folder/'records'
        env=base|{'A3_TRACE_INPUT':str(work/f'trace-{e}-{p}'),'A3_PROBE_OUTPUT':str(folder/'probe.json')}
        cli=[str(binary),'--storage',str(store),'record','synthetic','--','synthetic-no-target']
        if debug:
            cmd=['gdb','-q','-batch','-ex','set debuginfod enabled off','-ex','set pagination off',
                 '-ex','set disable-randomization off','-ex','source '+str(HERE/'probe_gdb.py'),'-ex','run','--args',*cli]
        else:
            cmd=['/usr/bin/time','-f','{"elapsed_s":%e,"user_s":%U,"sys_s":%S,"max_rss_kib":%M}',
                 '-o',str(folder/'time.json'),*cli]
        wall=command(cmd,env,folder/'stderr')
        # Actual product reader verifies the freshly finalized record; do not
        # replace it with a Python JSON parser or a hand-coded hash verifier.
        listing=subprocess.run([str(binary),'--storage',str(store),'list','--json'],env=env,cwd='/',
                               capture_output=True,text=True,timeout=30)
        assert listing.returncode==0,listing.stderr
        rows=json.loads(listing.stdout);assert len(rows)==1
        m=rows[0];assert m['parsed_events']==e and m['warnings']==[]
        assert m['state']=='completed' and m['storage_integrity']['status']=='verified'
        comparison=subprocess.run([str(binary),'--storage',str(store),'diff','synthetic','synthetic','--json'],
                                  env=env,cwd='/',capture_output=True,text=True,timeout=30)
        assert comparison.returncode==0
        d=json.loads(comparison.stdout);assert d['observed_differences'] is False
        assert all(q=={'capture':'complete','storage':'verified'} for q in d['reliability'].values())
        if debug:
            measured=json.loads((folder/'probe.json').read_text())
            assert measured['hits']=={'start':1,'end':1,'final':1},measured
            assert measured['exit_code']==0
            assert all(measured[k+'_frame']=='rundelta::capture::record' for k in ['start','end','final'])
            measured['gdb_record_wall_ms']=wall
        else:
            measured={'record_wall_ms':wall,'record_time':json.loads((folder/'time.json').read_text())}
            show=[str(binary),'--storage',str(store),'show','synthetic','--json']
            timed=['/usr/bin/time','-f','{"elapsed_s":%e,"user_s":%U,"sys_s":%S,"max_rss_kib":%M}',
                   '-o',str(folder/'show-time.json'),*show]
            measured['show_wall_ms']=command(timed,env,folder/'show-stderr')
            measured['show_time']=json.loads((folder/'show-time.json').read_text())
        return measured
    try:
        # Warmups are deliberately excluded and listed separately.
        for e,p in matrix:
            record(e,p,f'warm-{e}-{p}',False)
            print('WARMUP',e,p,flush=True)
        jobs=[(e,p,r) for e,p in matrix for r in range(args.repeats)]
        random.Random(20260920).shuffle(jobs)
        for e,p,r in jobs:
            debug=record(e,p,f'gdb-{e}-{p}-{r}',True)
            native=record(e,p,f'native-{e}-{p}-{r}',False)
            result['samples'].append(dict(events=e,processes=p,repeat=r,**debug,**native))
            (work/'results.json').write_text(json.dumps(result,indent=2))
            print('SAMPLE',e,p,r,'check_ms',round(debug['completion_wall_ms'],3),
                  'native_record_ms',round(native['record_wall_ms'],3),flush=True)
        for e,p in matrix:
            rows=[r for r in result['samples'] if r['events']==e and r['processes']==p]
            summary=fixtures[e,p]|{'samples':len(rows)}
            for key in ['completion_wall_ms','record_wall_ms','show_wall_ms']:
                summary[key]=stats([r[key] for r in rows])
            for key in ['start_memory_kib','end_memory_kib','final_memory_kib']:
                summary[key]=stats([r[key]['VmHWM'] for r in rows])
            for key in ['record_time','show_time']:
                summary[key+'_max_rss_kib']=stats([r[key]['max_rss_kib'] for r in rows])
            result['groups'].append(summary)
        result['warmups']=len(matrix)
        assert all(sha(ROOT/p)==h for p,h in before.items()),'tracked files changed during measurement'
        result['tracked_files_unchanged']=True
    except Exception as exc:
        result['failures'].append({'type':type(exc).__name__,'message':'measurement failed; inspect private logs; no failed sample discarded'})
        raise
    finally:
        (work/'results.json').write_text(json.dumps(result,indent=2)+'\n')
        print('RESULT_JSON:',work/'results.json',flush=True)
if __name__=='__main__':main()
