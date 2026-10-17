#!/usr/bin/env python3
"""Reproduce trace information loss in a fresh directory, without source edits.
All traces, binaries and records stay private in that directory. Counts vary.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

FILTER = 'execve,execveat,open,openat,openat2,access,faccessat,faccessat2,chdir,fchdir,clone,clone3,fork,vfork,unshare,setns,chroot,pivot_root'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True, help='read-only RunDelta checkout with existing dev dependencies')
    parser.add_argument('--strace', type=Path, required=True)
    parser.add_argument('--original-raw', type=Path, help='optional existing private SPAdes trace')
    parser.add_argument('--original-show', type=Path, help='optional saved show JSON for exact parser equivalence check')
    args = parser.parse_args()
    repo = args.repo.resolve(strict=True)
    st = args.strace.resolve(strict=True)
    rd = repo / 'target/debug/rundelta'
    work = Path(tempfile.mkdtemp(prefix='rundelta-attribution-probe-'))
    print('Private workspace:', work, flush=True)
    env = dict(PATH='/usr/bin:/bin', HOME=str(work), LANG='C', LC_ALL='C', RUNDELTA_STRACE=str(st))
    source = Path(__file__).with_name('probe.c')
    subprocess.run(['gcc', '-std=c11', '-O2', '-pthread', '-Wall', '-Wextra', str(source), '-o', str(work / 'probe')], check=True)
    rows = []
    plan = [
        ('joined-getpid', 'record', 16, 1, 0, 12),
        ('group-getpid', 'record', 16, 0, 0, 12),
        ('group-open', 'record', 16, 0, 1, 12),
        ('minimal', 'direct', 1, 0, 0, 24),
        ('standalone', 'direct', 16, 0, 0, 12),
        ('nested', 'nested', 16, 0, 0, 12),
        ('reduced-2', 'direct', 2, 0, 0, 24),
        ('reduced-4', 'direct', 4, 0, 0, 24),
    ]
    for group, mode, workers, joined, file_mode, samples in plan:
        for i in range(samples):
            name = f'{group}-{i:02d}'
            target = [str(work / 'probe'), str(joined), str(workers), str(file_mode)]
            trace = work / f'{name}.trace'
            if mode == 'record':
                command = [str(rd), '--storage', str(work / 'records'), 'record', name, '--'] + target
            else:
                command = [str(st), '-f', '-q', '-I', '2', '--kill-on-exit', '-xx', '-yy',
                           '-s', '65535', '-e', 'trace=' + FILTER, '-o', str(trace), '--'] + target
                if mode == 'nested':
                    command = [str(st), '-q', '-s', '512', '-e', 'trace=ptrace,wait4,waitid',
                               '-o', str(work / f'{name}.outer'), '--'] + command
            with (work / f'{name}.stdout').open('wb') as out, (work / f'{name}.stderr').open('wb') as err:
                process = subprocess.run(command, cwd=work, env=env, stdout=out, stderr=err, timeout=30)
            row = dict(group=group, name=name, workers=workers, exit_code=process.returncode)
            if mode == 'record':
                saved = subprocess.check_output([str(rd), '--storage', str(work / 'records'), 'show', name, '--json'], env=env)
                (work / f'{name}.show.json').write_bytes(saved)
                record = json.loads(saved)
                m = record['metadata']
                trace = work / 'records/runs' / m['id'] / 'raw/trace.log'
                row.update(warnings=m['warnings'], state=m['state'], completeness=m['completeness'],
                           target_code=m['exit_code'], collector_code=m['collector_exit_code'])
            lines = trace.read_text().splitlines()
            row['unknown'] = [dict(line=n, text=line) for n, line in enumerate(lines, 1) if '???(' in line]
            if mode == 'nested':
                outer = (work / f'{name}.outer').read_text().splitlines()
                row['ptrace_failures'] = [dict(line=n, text=line) for n, line in enumerate(outer, 1)
                                          if line.startswith('ptrace(') and '= -1' in line]
            rows.append(row)
        print(group, 'unknown runs', sum(bool(r['unknown']) for r in rows if r['group'] == group), '/', samples, flush=True)
    (work / 'results.json').write_text(json.dumps(rows, indent=2))
    if bool(args.original_raw) != bool(args.original_show):
        parser.error('supply both --original-raw and --original-show')
    if args.original_raw:
        # Compile the existing modules by path. No copying/patching product logic;
        # output is external and existing serde artifacts are read-only inputs.
        code = ('#![allow(dead_code)]\n#[path=' + json.dumps(str(repo / 'src/model/mod.rs')) + '] mod model;\n'
                '#[path=' + json.dumps(str(repo / 'src/parse/mod.rs')) + '] mod parse;\n'
                'fn main() { let a: Vec<String> = std::env::args().collect(); '
                'let raw=std::fs::read_to_string(&a[1]).unwrap(); '
                'let p=parse::parse_with_cwd(&raw,"raw/trace.log",a.get(2).map(String::as_str)); '
                'println!("{}",serde_json::json!({"events":p.events,"warnings":p.warnings,"lines":p.lines})); }\n')
        (work / 'parser_probe.rs').write_text(code)
        serde = list((repo / 'target/debug/deps').glob('libserde-*.rlib'))
        sj = list((repo / 'target/debug/deps').glob('libserde_json-*.rlib'))
        if len(serde) != 1 or len(sj) != 1:
            raise RuntimeError('ambiguous/missing existing dev dependencies; no rebuild attempted')
        subprocess.run(['rustc', '--edition=2024', str(work / 'parser_probe.rs'), '-L',
                        'dependency=' + str(repo / 'target/debug/deps'), '--extern', 'serde=' + str(serde[0]),
                        '--extern', 'serde_json=' + str(sj[0]), '-o', str(work / 'parser_probe')], check=True)
        saved = json.loads(args.original_show.read_text())
        result = subprocess.check_output([str(work / 'parser_probe'), str(args.original_raw.resolve()), saved['metadata']['cwd']])
        (work / 'parser-replay.json').write_bytes(result)
        actual = json.loads(result)
        assert actual['events'] == saved['events']
        assert actual['warnings'] == saved['metadata']['warnings']
        assert actual['lines'] == saved['metadata']['raw_lines']
        print('PASS: unmodified parser matches all stored events, warnings and line count')
    print('No unknown lines in a batch is a valid negative result. Keep all evidence; do not force a hit.')


if __name__ == '__main__':
    main()
