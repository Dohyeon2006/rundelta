#!/usr/bin/env python3
"""Run the CLONE_FS/CWD controls without changing the checkout or old records.
Requires Linux, gcc/pthreads and existing RunDelta/strace binaries.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

CASES = [
    ('normal', 'normal', 'none', 'A', 0),
    ('shared-stable', 'pthread', 'none', 'A', 0),
    ('shared-chdir', 'pthread', 'chdir', 'B', 0),
    ('private-chdir', 'fork', 'chdir', 'A', 0),
    ('shared-process-chdir', 'clonefs', 'chdir', 'B', 0),
    ('shared-fchdir', 'pthread', 'fchdir', 'B', 0),
    ('private-fchdir', 'fork', 'fchdir', 'A', 0),
    ('shared-fail-chdir', 'pthread', 'fail-chdir', 'A', 0),
    ('private-fail-chdir', 'fork', 'fail-chdir', 'A', 0),
    ('shared-fail-fchdir', 'pthread', 'fail-fchdir', 'A', 0),
    ('private-fail-fchdir', 'fork', 'fail-fchdir', 'A', 0),
    ('mixed-groups', 'mixed', 'chdir', 'A', 0),
    ('normal-access', 'normal', 'none', 'A', 1),
    ('shared-access', 'pthread', 'chdir', 'B', 1),
    ('private-access', 'fork', 'chdir', 'A', 1),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rundelta', required=True, type=Path)
    parser.add_argument('--strace', required=True, type=Path)
    args = parser.parse_args()
    rd, st = args.rundelta.resolve(strict=True), args.strace.resolve(strict=True)
    here = Path(__file__).resolve().parent
    work = Path(tempfile.mkdtemp(prefix='rundelta-clone-fs-replay-'))
    print('Private workspace:', work, flush=True)
    shutil.copyfile(here / 'PLAN.md', work / 'PLAN.md')
    for marker in ['A', 'B']:
        (work / marker).mkdir()
        (work / marker / 'payload.txt').write_text(marker + '\n')
    subprocess.run(['gcc', '-O2', '-std=c11', '-Wall', '-Wextra', '-Werror', '-pthread',
                    str(here / 'probe.c'), '-o', str(work / 'probe')], check=True)
    env = dict(PATH='/usr/bin:/bin', HOME=str(work), LANG='C', LC_ALL='C', RUNDELTA_STRACE=str(st))
    rows = []
    repeats = []
    for case, mode, operation, expected, access in CASES:
        for sample in range(3):
            name = f'{case}-{sample}'
            destination = work / ('missing-directory' if operation == 'fail-chdir' else 'B')
            target = [str(work / 'probe'), mode, operation, str(destination), expected, str(access)]
            with (work / f'{name}.stdout').open('wb') as out, (work / f'{name}.stderr').open('wb') as err:
                process = subprocess.run([str(rd), '--storage', str(work / 'records'), 'record', name, '--'] + target,
                                         cwd=work / 'A', env=env, stdout=out, stderr=err, timeout=30)
            saved = subprocess.check_output([str(rd), '--storage', str(work / 'records'), 'show', name, '--json'], env=env)
            (work / f'{name}.show.json').write_bytes(saved)
            run = json.loads(saved)
            m = run['metadata']
            trace = (work / 'records/runs' / m['id'] / 'raw/trace.log').read_text()
            payload = [e for e in run['events'] if e['kind'] == 'file' and e['operation'] == 'openat'
                       and (e.get('path') or '').endswith('/payload.txt')]
            with (work / f'{name}.show.txt').open('wb') as out:
                subprocess.run([str(rd), '--storage', str(work / 'records'), 'show', name], env=env, stdout=out, check=True)
            comparison = subprocess.run([str(rd), '--storage', str(work / 'records'), 'diff', 'normal-0', name, '--json'],
                                        env=env, capture_output=True)
            (work / f'{name}.diff.json').write_bytes(comparison.stdout)
            report = json.loads(comparison.stdout)
            rows.append(dict(name=name, case=case, sample=sample, record_exit=process.returncode,
                             target_exit=m['exit_code'], collector_exit=m['collector_exit_code'],
                             completeness=m['completeness'], storage=run['storage_integrity']['status'],
                             warnings=m['warnings'], clone_fs_raw='CLONE_FS' in trace,
                             expected_marker=expected, payload_events=payload,
                             expected_path_observed=len(payload) == 1 and payload[0]['path'] == str(work / expected / 'payload.txt'),
                             output=(work / f'{name}.stdout').read_text(), diff_exit=comparison.returncode,
                             diff_observed=report['observed_differences']))
        for format_name, flag in [('json', ['--json']), ('txt', [])]:
            result = subprocess.run([str(rd), '--storage', str(work / 'records'), 'diff', case + '-0', case + '-1'] + flag,
                                    env=env, capture_output=True)
            (work / f'{case}.repeat.{format_name}').write_bytes(result.stdout)
            if format_name == 'json':
                r = json.loads(result.stdout)
                repeats.append(dict(case=case, exit_code=result.returncode, observed_differences=r['observed_differences'],
                                    reliability=r['reliability']))
            else:
                assert result.returncode == repeats[-1]['exit_code']
        group = [r for r in rows if r['case'] == case]
        print(case, 'record exits', [r['record_exit'] for r in group],
              'completeness', [r['completeness'] for r in group], flush=True)
    (work / 'results.json').write_text(json.dumps(rows, indent=2))
    (work / 'repeats.json').write_text(json.dumps(repeats, indent=2))
    assert all(r['record_exit'] == 0 and r['expected_path_observed'] for r in rows), 'Keep failures; do not relax assertions'
    print('PASS: 45 target observations and path checks; raw evidence is private. No product or old records changed.')


if __name__ == '__main__':
    main()
