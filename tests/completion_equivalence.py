#!/usr/bin/env python3
"""Completion contract through record/show/diff, using an explicit synthetic backend.
No real target is run. Optional --snapshot captures all nonvolatile persisted fields
for byte-for-byte old/new comparison; expectations below predate the optimization.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]

def exec_event(pid='20', result='0'):
    return f'{pid} execve("/synthetic/target", [], 0) = {result}'

def exit_event(pid='20', code=0):
    return f'{pid} +++ exited with {code} +++'

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT/'target/debug/rundelta')
    parser.add_argument('--snapshot', type=Path)
    args = parser.parse_args()
    # name, trace lines, collector status (negative = signal), target code,
    # target signal, raw exit, missing generations, record state, completeness,
    # required warning substrings
    cases = [
        ('single', [exec_event(), exit_event()], 0, 0, None, exit_event()[3:], [], 'completed', 'complete_for_supported_events', ()),
        ('fork_child_first', [exec_event(), '20 fork() = 3', exit_event('3'), exit_event()], 0, 0, None, exit_event()[3:], [], 'completed', 'complete_for_supported_events', ()),
        ('fork_root_first', [exec_event(), '20 fork() = 3', exit_event(), exit_event('3')], 0, 0, None, exit_event()[3:], [], 'completed', 'complete_for_supported_events', ()),
        ('execs', [exec_event('9', '-1 ENOENT (No such file or directory)'), exec_event(), exec_event('20'), exit_event('9'), exit_event('20', 7)], 7, 7, None, '+++ exited with 7 +++', [], 'completed', 'partial', ('Unintroduced',)),
        ('roots', [exec_event(), exec_event('3'), exit_event('20', 7), exit_event('3')], 7, 7, None, '+++ exited with 7 +++', [], 'completed', 'partial', ('Unintroduced',)),
        ('signal', [exec_event(), '20 +++ killed by SIGTERM +++'], -signal.SIGTERM, None, signal.SIGTERM, '+++ killed by SIGTERM +++', [], 'completed', 'complete_for_supported_events', ()),
        ('realtime', [exec_event(), '20 +++ killed by SIGRT_2 +++'], -34, None, 34, '+++ killed by SIGRT_2 +++', [], 'completed', 'complete_for_supported_events', ()),
        ('unknown_signal', [exec_event(), '20 +++ killed by SIGFUTURE +++'], 0, None, None, '+++ killed by SIGFUTURE +++', [], 'completed', 'partial', ('unrecognized target termination signal',)),
        ('return_143', [exec_event(), exit_event('20', 143)], 143, 143, None, '+++ exited with 143 +++', [], 'completed', 'complete_for_supported_events', ()),
        ('missing_root', [exec_event()], 0, None, None, None, ['20'], 'capture_failed', 'incomplete', ('root process completion not observed',)),
        ('missing_children', [exec_event(), '20 fork() = 3', '20 fork() = 100', exit_event()], 0, 0, None, exit_event()[3:], ['100', '3'], 'completed', 'partial', ()),
        ('fresh_reuse_missing', [exec_event(), '20 fork() = 3', exit_event('3'), '20 fork() = 3', exit_event()], 0, 0, None, exit_event()[3:], ['3#2'], 'completed', 'partial', ()),
        ('fresh_reuse_complete', [exec_event(), '20 fork() = 3', exit_event('3'), '20 fork() = 3', exit_event('3'), exit_event()], 0, 0, None, exit_event()[3:], [], 'completed', 'complete_for_supported_events', ()),
        ('live_collision', [exec_event(), '20 fork() = 3', '20 fork() = 3', exit_event('3'), exit_event()], 0, 0, None, exit_event()[3:], ['3#1'], 'completed', 'partial', ('LiveCollision',)),
        ('unintroduced', [exec_event(), '3 open("/synthetic/input", O_RDONLY) = 3', exit_event('3'), exit_event()], 0, 0, None, exit_event()[3:], [], 'completed', 'partial', ('Unintroduced',)),
        ('interleaved', [exec_event(), '20 fork() = 3', '3 open("/synthetic/input", O_RDONLY) = 3', '20 fork() = 100', exit_event('3'), '100 open("/synthetic/input", O_RDONLY) = 3', exit_event(), exit_event('100')], 0, 0, None, exit_event()[3:], [], 'completed', 'complete_for_supported_events', ()),
        ('repeated_exit_generation', [exec_event(), exit_event('20', 4), exit_event('20', 7)], 4, 4, None, '+++ exited with 4 +++', [], 'completed', 'partial', ('Reappeared',)),
        ('exit_before_exec', [exit_event(), exec_event()], 0, None, None, None, ['20#2'], 'capture_failed', 'incomplete', ('Reappeared', 'root process completion not observed')),
        ('unfinished_child_before_resume', [exec_event(), '20 clone(flags=SIGCHLD <unfinished ...>', '3 open("/synthetic/input", O_RDONLY) = 3', '20 <... clone resumed>) = 3', exit_event('3'), exit_event()], 0, 0, None, exit_event()[3:], [], 'completed', 'complete_for_supported_events', ()),
        ('evidence_order_root', ['20 execve("/synthetic/target", [], 0 <unfinished ...>', exec_event('3'), '20 <... execve resumed>) = 0', exit_event('20', 7), exit_event('3')], 7, 7, None, '+++ exited with 7 +++', [], 'completed', 'partial', ('Unintroduced',)),
        ('no_root', [exit_event()], 0, None, None, None, [], 'capture_failed', 'incomplete', ('root process completion not observed',)),
        ('empty', [], 0, None, None, None, [], 'capture_failed', 'incomplete', ('root process completion not observed',)),
        ('collector_mismatch', [exec_event(), exit_event()], 9, 0, None, exit_event()[3:], [], 'capture_failed', 'incomplete', ('collector status does not confirm target completion',)),
        ('collector_signal', [exec_event(), exit_event()], -signal.SIGTERM, 0, None, exit_event()[3:], [], 'capture_failed', 'incomplete', ('collector status does not confirm target completion',)),
        ('failed_spawn', [exec_event(), '20 fork() = -1 EAGAIN (Resource temporarily unavailable)', exit_event()], 0, 0, None, exit_event()[3:], [], 'completed', 'complete_for_supported_events', ()),
        ('unknown_event', [exec_event(), '3 unsupported() = 0', exit_event()], 0, 0, None, exit_event()[3:], [], 'completed', 'partial', ('unsupported or incomplete event',)),
    ]
    snapshots = {}
    with tempfile.TemporaryDirectory(prefix='completion-contract-') as temp:
        work = Path(temp)
        backend = work/'backend'
        backend.write_text('#!/usr/bin/env python3\nimport os,sys,shutil,signal\nif "--version" in sys.argv[1:]:\n print("completion contract synthetic backend; no target")\nelse:\n shutil.copyfile(os.environ["TRACE"],sys.argv[sys.argv.index("-o")+1])\n status=int(os.environ["STATUS"])\n if status<0:\n  signal.signal(-status,signal.SIG_DFL)\n  os.kill(os.getpid(),-status)\n sys.exit(status)\n')
        backend.chmod(0o700)
        for name, lines, status, code, sig, raw, missing, state, completeness, warnings in cases:
            trace = work/'trace'
            trace.write_text('\n'.join(lines)+ ('\n' if lines else ''))
            env = os.environ | {'RUNDELTA_STRACE':str(backend), 'TRACE':str(trace), 'STATUS':str(status)}
            def run(*cli):
                return subprocess.run([str(args.binary.resolve()), '--storage', str(work/name), *cli], env=env, cwd='/', capture_output=True, text=True, timeout=15)
            recorded = run('record', 'case', '--', '/synthetic/target')
            expected_rc = 125 if state == 'capture_failed' else (128+sig if sig else code if code is not None else status)
            assert recorded.returncode == expected_rc, (name, recorded.returncode, recorded.stderr)
            shown = run('show', 'case', '--json')
            assert shown.returncode == 0, (name, shown.stderr)
            data = json.loads(shown.stdout); m = data['metadata']
            assert (m['exit_code'], m['signal'], m['target_exit_raw'], m['state']) == (code,sig,raw,state), (name,m)
            assert (m['collector_exit_code'],m['collector_signal']) == ((status,None) if status>=0 else (None,-status)), (name,m)
            completion = [w for w in m['warnings'] if w.startswith('process completion not observed for PIDs:')]
            assert completion == (['process completion not observed for PIDs: '+', '.join(missing)] if missing else []), (name,m)
            for warning in warnings:
                assert any(warning in w for w in m['warnings']), (name,warning,m)
            assert m['completeness'] == completeness, (name,m)
            assert data['storage_integrity']['status'] == 'verified'
            comparison = run('diff','case','case','--json')
            assert comparison.returncode == (0 if m['completeness']=='complete_for_supported_events' else 3), (name,comparison.stdout)
            d = json.loads(comparison.stdout)
            assert d['observed_differences'] is False
            for key in ['id','started','ended']: m.pop(key)
            snapshots[name] = {'record_rc':recorded.returncode, 'run':data, 'diff_rc':comparison.returncode, 'reliability':d['reliability']}
            print('PASS', name)
    if args.snapshot: args.snapshot.write_text(json.dumps(snapshots, indent=2, sort_keys=True)+'\n')
    print(f'PASS: {len(cases)} completion contracts')

if __name__ == '__main__': main()
